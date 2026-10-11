import { useCallback, useEffect, useRef, useState } from "react";
import { agentBridge, type SkillEntry } from "@/lib/bridge";

export type SkillSlashState = {
  /** 是否展示候选浮层（输入形如 `/query` 时打开）。 */
  open: boolean;
  /** 当前查询（`/` 之后的文本）。 */
  query: string;
  /** 过滤后的候选。 */
  results: SkillEntry[];
  error: string | null;
  selectedIndex: number;
  /** 随输入同步：`/xxx`（无空白）打开并更新查询，否则关闭。 */
  sync: (text: string) => void;
  moveSelection: (delta: number) => void;
  /** 确认当前高亮项（浮层不可确认时返回 null）。 */
  confirmSelection: () => SkillEntry | null;
  close: () => void;
  /** 某个技能名是否存在（提交时判断 `/name args` 是否是技能入口）。 */
  hasSkill: (name: string) => boolean;
};

/** 只在整段输入形如 `/xxx`（无空白）时算作 slash 候选。 */
const SLASH_PATTERN = /^\/([^\s]*)$/;

/**
 * `/` 技能候选项的无头控制器：只管「候选浮层 + 过滤」，不碰输入框 DOM。
 *
 * 技能目录随项目（工作区）变化，切换项目时重新拉取。过滤只在前端做——
 * 目录很小，不必打后端。提交时的**激活**走 `agentBridge().activateSkill`。
 */
export function useSkillSlashSearch(project: string | null): SkillSlashState {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [skills, setSkills] = useState<SkillEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [selectedIndex, setSelectedIndex] = useState(0);

  // 加载当前项目的技能目录。
  useEffect(() => {
    if (!project) {
      setSkills([]);
      return;
    }
    let alive = true;
    void (async () => {
      try {
        const entries = await (await agentBridge()).listSkills(project);
        if (alive) {
          setSkills(entries);
          setError(null);
        }
      } catch (reason) {
        if (alive) {
          setError(reason instanceof Error ? reason.message : String(reason));
          setSkills([]);
        }
      }
    })();
    return () => {
      alive = false;
    };
  }, [project]);

  const results = (() => {
    const q = query.trim().toLowerCase();
    if (!q) return skills;
    return skills.filter(
      (skill) =>
        skill.name.toLowerCase().includes(q) ||
        skill.description.toLowerCase().includes(q),
    );
  })();

  const sync = useCallback((text: string) => {
    const match = SLASH_PATTERN.exec(text);
    if (!match) {
      setOpen(false);
      return;
    }
    setOpen(true);
    setQuery(match[1]);
    setSelectedIndex(0);
  }, []);

  const close = useCallback(() => setOpen(false), []);

  const moveSelection = useCallback(
    (delta: number) => {
      setSelectedIndex((current) => {
        if (results.length === 0) return 0;
        return (current + delta + results.length) % results.length;
      });
    },
    [results.length],
  );

  const confirmSelection = useCallback((): SkillEntry | null => {
    return results[selectedIndex] ?? null;
  }, [results, selectedIndex]);

  const hasSkill = useCallback(
    (name: string) => skills.some((skill) => skill.name === name),
    [skills],
  );

  // 查询变化后若高亮越界，归零。
  const lastQueryRef = useRef(query);
  useEffect(() => {
    if (lastQueryRef.current !== query) {
      lastQueryRef.current = query;
      setSelectedIndex(0);
    }
  }, [query]);

  return {
    open,
    query,
    results,
    error,
    selectedIndex,
    sync,
    moveSelection,
    confirmSelection,
    close,
    hasSkill,
  };
}
