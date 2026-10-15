import { useCallback, useEffect, useRef, useState } from "react";
import { agentBridge, type SkillEntry } from "@/lib/bridge";

export type SkillSlashState = {
  /** Whether the candidate popover is shown (opens when input looks like `/query`). */
  open: boolean;
  /** Current query (the text after `/`). */
  query: string;
  /** Filtered candidates. */
  results: SkillEntry[];
  error: string | null;
  selectedIndex: number;
  /** Syncs with input: `/xxx` (no whitespace) opens and updates the query, otherwise closes. */
  sync: (text: string) => void;
  moveSelection: (delta: number) => void;
  /** Confirms the currently highlighted item (returns null when the popover cannot confirm). */
  confirmSelection: () => SkillEntry | null;
  close: () => void;
  /** Whether a skill name exists (used on submit to tell if `/name args` is a skill entry). */
  hasSkill: (name: string) => boolean;
};

/** Only counts as a slash candidate when the whole input looks like `/xxx` (no whitespace). */
const SLASH_PATTERN = /^\/([^\s]*)$/;

/**
 * Headless controller for `/` skill candidates: only handles "candidate popover + filter",
 * never touches the input DOM.
 *
 * The skill catalog varies per project (workspace) and is re-fetched when the project
 * changes. Filtering is done on the frontend only — the catalog is small, so there is no
 * need to hit the backend. **Activation** on submit goes through `agentBridge().activateSkill`.
 */
export function useSkillSlashSearch(project: string | null): SkillSlashState {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [skills, setSkills] = useState<SkillEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [selectedIndex, setSelectedIndex] = useState(0);

  // Load the skill catalog for the current project.
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

  // Reset the highlight to zero if it goes out of range after the query changes.
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
