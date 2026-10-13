import { useCallback, useEffect, useRef, useState } from "react";
import { LoaderCircle } from "lucide-react";
import { DesktopChrome } from "@/components/DesktopChrome";
import {
  AiConversationTranscript,
  type AiTranscriptMessage,
} from "@/components/ai/AiConversationTranscript";
import { AiChatComposer } from "@/components/ai/AiChatComposer";
import { AssistantMessage } from "@/components/AssistantMessage";
import { TaskCard } from "@/components/ai/TaskCard";
import { ModelSelector } from "@/components/ai/ModelSelector";
import { SessionSidebar } from "@/components/ai/SessionSidebar";
import { AiConversationSkeleton } from "@/components/ai/AiConversationSkeleton";
import { AiConversationMinimap } from "@/components/ai/AiConversationMinimap";
import { InlineReferences } from "@/lib/file-mentions/FileChips";
import { FileMentionPopover } from "@/lib/file-mentions/FileMentionPopover";
import {
  MentionEditor,
  type MentionEditorHandle,
} from "@/lib/file-mentions/MentionEditor";
import { useFileMentionSearch } from "@/lib/file-mentions/useFileMentionSearch";
import { useSkillSlashSearch } from "@/lib/slash/useSkillSlashSearch";
import { SkillSlashPopover } from "@/lib/slash/SkillSlashPopover";
import { CHIP_TOKEN, stripTokens } from "@/lib/file-mentions/editor-dom";
import type { FileReference } from "@/lib/file-mentions/types";
import { agentBridge, type ProjectEntry, type SessionSubscription } from "@/lib/bridge";
import type { AgentEventWire, SessionSnapshot, TaskViewWire, UsageWire } from "@/types/wire";
import type { AiSegment, AiToolExecution } from "@/types/ai";

let messageSeq = 0;
const nextId = () => `m${++messageSeq}`;

/** 侧边栏折叠偏好（仅本地 UI 状态，不跨端 / 不落库）。 */
const SIDEBAR_STORAGE_KEY = "shirley.desktop.sidebar-collapsed";

function readBool(key: string, fallback: boolean): boolean {
  try {
    const raw = window.localStorage.getItem(key);
    return raw === null ? fallback : raw === "1";
  } catch {
    return fallback;
  }
}

function writeBool(key: string, value: boolean) {
  try {
    window.localStorage.setItem(key, value ? "1" : "0");
  } catch {
    // localStorage 不可用（隐私模式等）时静默降级：状态仅当次会话有效。
  }
}

/** 视口窄于该宽度时自动收起侧边栏（sidebar 256px + 内容 760px 已放不下）。 */
const SIDEBAR_AUTO_COLLAPSE_PX = 900;

/**
 * 单个会话的视图状态（transcript + 每轮段落 + busy / 错误）。
 *
 * 多项目多会话：事件按 `project` + `session` 打标，每个会话各持一份视图；当前会话
 * 渲染，后台会话照常累加（不因切走丢上下文）。切换会话 = 换渲染哪一份 + 换订阅哪个
 * 事件流。视图键是 `project::session`——会话名只在项目内唯一，跨项目会重名。
 */
type SessionView = {
  messages: AiTranscriptMessage[];
  turnSegments: Record<string, AiSegment[]>;
  busy: boolean;
  error: string;
  /** 历史快照尚未到达：先乐观展示，渲染骨架屏。 */
  loading: boolean;
  /** 任务账本快照（`null` = 空账本 / 无账本）。驱动任务卡与状态行。 */
  todo: TaskViewWire | null;
  /** 正在压缩上下文（`compression_started` → `compression_finished`）。 */
  compressing: boolean;
  /** 上下文占用（used / limit tokens），来自 `context_usage` 事件。 */
  contextUsage: { used: number; limit: number } | null;
  /** 最近一次上报的用量，来自 `usage` 事件。 */
  usage: UsageWire | null;
};

function emptyView(): SessionView {
  return {
    messages: [],
    turnSegments: {},
    busy: false,
    error: "",
    loading: false,
    todo: null,
    compressing: false,
    contextUsage: null,
    usage: null,
  };
}

/** 视图键：跨项目不重名。 */
const viewKey = (project: string, session: string) => `${project}::${session}`;

type SessionLocation = { project: string; session: string };
type NavigationHistory = { entries: SessionLocation[]; index: number };

function removeLocations(history: NavigationHistory, remove: (entry: SessionLocation) => boolean): NavigationHistory {
  const entries = history.entries.filter((entry) => !remove(entry));
  const index = history.entries.slice(0, history.index + 1).filter((entry) => !remove(entry)).length - 1;
  return { entries, index };
}

/**
 * 运行态状态行：压缩中 / 上下文占用。放在输入框上方、任务卡下方。
 *
 * 对标 TUI footer 的上下文占用提示：desktop 此前完全不处理 `compression_*` /
 * `context_usage` 事件，压缩期间界面静默、用户不知道发生了什么。这里给一个
 * 轻量、不抢戏的反馈——没有可展示信息时整行不占位。
 */
function SessionStatusLine({
  compressing,
  contextUsage,
}: {
  compressing: boolean;
  contextUsage: { used: number; limit: number } | null;
}) {
  const percent =
    contextUsage && contextUsage.limit > 0
      ? Math.round((contextUsage.used / contextUsage.limit) * 100)
      : null;
  if (!compressing && percent === null) return null;
  return (
    <div className="mx-3 mb-1 flex items-center gap-2 text-caption text-muted">
      {compressing && (
        <span className="inline-flex items-center gap-1.5">
          <LoaderCircle className="size-3.5 animate-spin" aria-hidden="true" />
          正在压缩上下文
        </span>
      )}
      {percent !== null && (
        <span className="font-utility">
          上下文 {percent}%
        </span>
      )}
    </div>
  );
}

export function App() {
  // 每个会话一份视图（后台会话也保留，切回不丢），键为 `project::session`。
  const [views, setViews] = useState<Record<string, SessionView>>({});
  // 项目列表（活跃）与当前项目。
  const [projects, setProjects] = useState<ProjectEntry[]>([]);
  const [activeProject, setActiveProject] = useState<string | null>(null);
  const [sessionName, setSessionName] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [references, setReferences] = useState<FileReference[]>([]);
  const [model, setModel] = useState("");
  // UI 级错误（项目 / 会话 / 模型切换失败）；运行期错误进各会话视图的 `error`。
  const [uiError, setUiError] = useState("");
  const [navigationHistory, setNavigationHistory] = useState<NavigationHistory>({ entries: [], index: -1 });
  const [navigationBusy, setNavigationBusy] = useState(false);
  const [sessionTitles, setSessionTitles] = useState<Record<string, string>>({});
  const updateCurrentTitle = useCallback((title: string) => {
    if (activeProject && sessionName) {
      const key = viewKey(activeProject, sessionName);
      setSessionTitles((prev) => prev[key] === title ? prev : { ...prev, [key]: title });
    }
  }, [activeProject, sessionName]);

  useEffect(() => {
    if (!activeProject || !sessionName) return;
    setNavigationHistory((prev) => {
      const current = prev.entries[prev.index];
      if (current?.project === activeProject && current.session === sessionName) return prev;
      const entries = [...prev.entries.slice(0, prev.index + 1), { project: activeProject, session: sessionName }].slice(-100);
      return { entries, index: entries.length - 1 };
    });
  }, [activeProject, sessionName]);
  // 侧边栏折叠：用户手动偏好（持久化）。窗口过窄时自动置为收起，之后仍可手动展开。
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() =>
    readBool(SIDEBAR_STORAGE_KEY, false),
  );

  const toggleSidebar = useCallback(() => {
    setSidebarCollapsed((prev) => {
      const next = !prev;
      writeBool(SIDEBAR_STORAGE_KEY, next);
      return next;
    });
  }, []);

  // 窗口放不下时自动收起：跨入窄视口即折叠（不自动弹回，尊重用户之后的显式选择）。
  useEffect(() => {
    const query = window.matchMedia(`(max-width: ${SIDEBAR_AUTO_COLLAPSE_PX}px)`);
    const apply = (matches: boolean) => {
      if (matches) setSidebarCollapsed(true);
    };
    apply(query.matches);
    const onChange = (event: MediaQueryListEvent) => apply(event.matches);
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  }, []);

  const mentionSearch = useFileMentionSearch(activeProject);
  const slashSearch = useSkillSlashSearch(activeProject);
  const editorRef = useRef<MentionEditorHandle>(null);
  // 对话轨迹（minimap）要挂到 parent content（section）的左侧边缘，需要共享滚动视口。
  const transcriptViewportRef = useRef<HTMLDivElement>(null);
  // 当前订阅句柄（切走时退订）。
  const subRef = useRef<SessionSubscription | null>(null);
  // 每个会话当前在跑的 assistant 轮次 id（事件按它落点），键为 `project::session`。
  const turnRef = useRef<Map<string, string>>(new Map());
  // 每个会话本轮的起始时间戳（算「处理用时 Ns」），键为 `project::session`。
  const turnStartRef = useRef<Map<string, number>>(new Map());
  // 快照应用前缓冲事件（订阅与快照之间到达的增量不丢）。
  const readyRef = useRef<Set<string>>(new Set());
  const bufferRef = useRef<Map<string, AgentEventWire[]>>(new Map());

  const reloadProjects = useCallback(async () => {
    try {
      const bridge = await agentBridge();
      const active = await bridge.listProjects(false);
      setProjects(active);
      return active;
    } catch (e) {
      setUiError(e instanceof Error ? e.message : String(e));
      return [] as ProjectEntry[];
    }
  }, []);

  // 把一个事件应用到指定会话的视图（纯函数式更新，避免闭包过期）。
  const applyEvent = useCallback((key: string, event: AgentEventWire) => {
    // 用户点击停止：后端**保留**本轮（用户消息 + 半截流式回复）作为下一轮参考，
    // 不再回退删除（需求 2），并以 `finished(cancelled)` 收尾。这里只把本轮
    // assistant 从 streaming 标记为 complete、清掉 `busy`——**不丢弃条目、不回退输入**
    // （与 TUI `App::complete_turn` 的打断语义一致）。
    if (event.type === "finished" && event.stop_reason === "cancelled") {
      const startedAt = turnStartRef.current.get(key);
      const durationMs = startedAt !== undefined ? Math.max(0, Date.now() - startedAt) : undefined;
      turnRef.current.delete(key);
      turnStartRef.current.delete(key);
      setViews((prev) => {
        const view = prev[key] ?? emptyView();
        return {
          ...prev,
          [key]: {
            ...view,
            messages: view.messages.map((message) =>
              message.status === "streaming" ? { ...message, status: "complete", durationMs, stopReason: "cancelled" } : message,
            ),
            turnSegments: Object.fromEntries(Object.entries(view.turnSegments).map(([id, segments]) => [
              id,
              segments.map((segment) => segment.kind === "tools" ? {
                ...segment,
                executions: segment.executions.map((execution) => execution.status === "running" ? {
                  ...execution, status: "error" as const, error: "已中断，执行状态需确认",
                } : execution),
              } : segment),
            ])),
            busy: false,
            compressing: false,
            error: "",
          },
        };
      });
      return;
    }
    const needsTurn =
      event.type === "content_delta" ||
      event.type === "compact" ||
      event.type === "tool_started" ||
      event.type === "tool_finished";
    let assistantId = turnRef.current.get(key) ?? null;
    if (needsTurn && !assistantId) {
      assistantId = nextId();
      turnRef.current.set(key, assistantId);
    }
    const finishing = event.type === "finished" || event.type === "error";
    const startedAt = turnStartRef.current.get(key);
    const durationMs = finishing && startedAt !== undefined ? Math.max(0, Date.now() - startedAt) : undefined;
    if (finishing) {
      turnRef.current.delete(key);
      turnStartRef.current.delete(key);
    }
    setViews((prev) => {
      const view = prev[key] ?? emptyView();
      let next = view;
      if (
        needsTurn &&
        assistantId &&
        !view.messages.some((message) => message.id === assistantId)
      ) {
        next = {
          ...view,
          messages: [
            ...view.messages,
            { id: assistantId, role: "assistant", content: "", status: "streaming" },
          ],
          turnSegments: { ...view.turnSegments, [assistantId]: [] },
        };
      }
      let updated = applyWireEvent(next, event, assistantId);
      // 轮次收尾（完成 / 出错）：把本轮耗时写到 assistant 消息上，供「处理用时 Ns」折叠。
      if (finishing && assistantId) {
        if (durationMs !== undefined) {
          updated = {
            ...updated,
            messages: updated.messages.map((message) =>
              message.id === assistantId ? { ...message, durationMs } : message,
            ),
          };
        }
      }
      return { ...prev, [key]: updated };
    });
  }, []);

  // 先响应交互：立刻把前台会话切过去并进入加载态（渲染骨架屏），不等后端往返。
  // 打开会话 / 切换会话都在取历史快照**之前**调它——历史快照要走一次
  // `agent_subscribe` 往返，不能让用户对着旧会话干等。
  const beginSessionLoad = useCallback((project: string, session: string) => {
    const key = viewKey(project, session);
    setSessionName(session);
    setViews((prev) => {
      const existing = prev[key];
      // 已有缓存内容（后台会话跑过）直接复用、不闪骨架；否则占位加载。
      if (existing && existing.messages.length > 0) {
        return { ...prev, [key]: { ...existing, loading: true } };
      }
      return { ...prev, [key]: { ...emptyView(), loading: true } };
    });
  }, []);

  // 订阅一个会话：退订旧的，拿快照重建视图，再叠加之后的事件。
  const openSession = useCallback(
    async (project: string, session: string) => {
      const key = viewKey(project, session);
      beginSessionLoad(project, session);
      const bridge = await agentBridge();
      subRef.current?.unsubscribe();
      subRef.current = null;
      readyRef.current.delete(key);
      bufferRef.current.delete(key);
      turnRef.current.delete(key);
      let sub;
      try {
        sub = await bridge.openSession(project, session, (event) => {
          // 快照尚未应用：先缓冲，待快照落地后按序补放（不丢、不重）。
          if (!readyRef.current.has(key)) {
            const buffer = bufferRef.current.get(key) ?? [];
            buffer.push(event);
            bufferRef.current.set(key, buffer);
            return;
          }
          applyEvent(key, event);
        });
      } catch (e) {
        // 订阅失败：退出加载态并如实报错，别让骨架屏一直转下去。
        const message = e instanceof Error ? e.message : String(e);
        setViews((prev) => ({
          ...prev,
          [key]: { ...(prev[key] ?? emptyView()), loading: false, error: message },
        }));
        return;
      }
      subRef.current = sub;
      const { view, lastAssistantId } = viewFromSnapshot(sub.snapshot);
      if (lastAssistantId) turnRef.current.set(key, lastAssistantId);
      if (!sub.snapshot.running) turnStartRef.current.delete(key);
      setViews((prev) => {
        const cached = prev[key];
        const turnSegments = { ...view.turnSegments };
        const messages = view.messages.map((message, index) => {
          const previous = cached?.messages[index];
          if (message.role !== "assistant" || previous?.role !== "assistant" || !message.content.startsWith(previous.content)) return message;
          const cachedTools = new Map(
            (cached.turnSegments[String(previous.id)] ?? []).flatMap((segment) =>
              segment.kind === "tools" ? segment.executions.map((execution) => [execution.id, execution] as const) : [],
            ),
          );
          turnSegments[String(message.id)] = (turnSegments[String(message.id)] ?? []).map((segment) =>
            segment.kind === "tools" ? {
              ...segment,
              executions: segment.executions.map((execution) => {
                const old = cachedTools.get(execution.id);
                return old && old.tool_name === execution.tool_name && old.status !== "running" && old.status !== "pending"
                  && JSON.stringify(old.input) === JSON.stringify(execution.input) ? old : execution;
              }),
            } : segment,
          );
          return {
            ...message,
            durationMs: previous.durationMs,
            stopReason: previous.stopReason,
          };
        });
        return {
          ...prev,
          [key]: {
            ...view,
            messages,
            turnSegments,
          },
        };
      });
      readyRef.current.add(key);
      const buffered = bufferRef.current.get(key) ?? [];
      bufferRef.current.delete(key);
      for (const event of buffered) applyEvent(key, event);
    },
    [applyEvent, beginSessionLoad],
  );

  // 选中一个项目：切前台 + 打开它当前会话。
  const selectProject = useCallback(
    async (project: string) => {
      try {
        setUiError("");
        if (project === activeProject) return;
        const bridge = await agentBridge();
        subRef.current?.unsubscribe();
        subRef.current = null;
        setSessionName(null);
        setActiveProject(project);
        await bridge.modelName(project).then(setModel).catch(() => setModel(""));
        const name = await bridge.currentSession(project).catch(() => null);
        if (name) await openSession(project, name).catch(() => {});
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject, openSession],
  );

  // 启动：载入项目列表，选第一个（最近打开）并打开它当前会话。
  useEffect(() => {
    void (async () => {
      const bridge = await agentBridge();
      const active = await reloadProjects();
      const first = active[0]?.id ?? null;
      if (!first) return;
      setActiveProject(first);
      await bridge.modelName(first).then(setModel).catch(() => {});
      const name = await bridge.currentSession(first).catch(() => null);
      if (name) await openSession(first, name).catch(() => {});
    })();
  }, [reloadProjects, openSession]);

  const addProject = useCallback(async () => {
    try {
      setUiError("");
      const bridge = await agentBridge();
      const entry = await bridge.addProject();
      if (!entry) return; // 用户取消
      await reloadProjects();
      await selectProject(entry.id);
    } catch (e) {
      setUiError(e instanceof Error ? e.message : String(e));
    }
  }, [reloadProjects, selectProject]);

  const renameProject = useCallback(
    async (project: string, name: string) => {
      try {
        setUiError("");
        await (await agentBridge()).renameProject(project, name);
        await reloadProjects();
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [reloadProjects],
  );

  const archiveProject = useCallback(
    async (project: string) => {
      try {
        setUiError("");
        await (await agentBridge()).archiveProject(project);
        setNavigationHistory((prev) => removeLocations(prev, (entry) => entry.project === project));
        // 归档的是当前项目：清空前台，切到下一个可用项目（可能为空）。
        if (project === activeProject) {
          subRef.current?.unsubscribe();
          subRef.current = null;
          setSessionName(null);
          setActiveProject(null);
          const active = await reloadProjects();
          const next = active[0]?.id ?? null;
          if (next) await selectProject(next);
        } else {
          await reloadProjects();
        }
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject, reloadProjects, selectProject],
  );

  const switchSession = useCallback(
    async (project: string, name: string) => {
      try {
        setUiError("");
        // 跨项目直达目标会话，避免中途打开该项目的旧会话、污染导航历史。
        setActiveProject(project);
        beginSessionLoad(project, name);
        const bridge = await agentBridge();
        await bridge.switchSession(project, name);
        await openSession(project, name);
        await bridge.modelName(project).then(setModel);
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [openSession, beginSessionLoad],
  );

  const navigateHistory = useCallback(async (offset: number) => {
    const index = navigationHistory.index + offset;
    const target = navigationHistory.entries[index];
    if (!target || navigationBusy) return;
    setNavigationBusy(true);
    try {
      const bridge = await agentBridge();
      // 直接交给后端查找：未发送消息的惰性会话只在内存里，不会出现在磁盘列表。
      // 切换成功后才移动历史游标；失败时仍留在原会话，可重试。
      await bridge.switchSession(target.project, target.session);
      setNavigationHistory((prev) => ({ ...prev, index }));
      setActiveProject(target.project);
      await openSession(target.project, target.session);
      await bridge.modelName(target.project).then(setModel).catch(() => setModel(""));
      setUiError("");
    } catch (error) {
      setUiError(error instanceof Error ? error.message : String(error));
    } finally {
      setNavigationBusy(false);
    }
  }, [navigationHistory, navigationBusy, openSession]);

  // 在指定项目下新建会话（项目行上的「＋」可直接作用于任意项目，不限于前台）。
  const newSession = useCallback(
    async (project: string) => {
      try {
        setUiError("");
        const bridge = await agentBridge();
        // 不带标题：名字在首条消息后由 AI 自动生成，用户想改再手动重命名。
        const entry = await bridge.newSession(project, null);
        setActiveProject(project);
        beginSessionLoad(project, entry.name);
        await openSession(project, entry.name);
        await bridge.modelName(project).then(setModel);
        // 直接聚焦输入框——新建会话的意图是"马上开始聊"，而不是先做管理。
        editorRef.current?.focus();
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [openSession, beginSessionLoad],
  );

  const renameSession = useCallback(
    async (project: string, name: string, title: string) => {
      try {
        setUiError("");
        await (await agentBridge()).renameSession(project, name, title);
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [],
  );

  const deleteSession = useCallback(
    async (project: string, name: string) => {
      try {
        setUiError("");
        const bridge = await agentBridge();
        await bridge.deleteSession(project, name);
        setNavigationHistory((prev) => removeLocations(prev, (entry) => entry.project === project && entry.session === name));
        // 删的若是前台会话，重新打开后端选中的会话（可能为空）。
        if (project === activeProject && name === sessionName) {
          const current = await bridge.currentSession(project).catch(() => null);
          if (current) await openSession(project, current);
          else {
            subRef.current?.unsubscribe();
            subRef.current = null;
            setSessionName(null);
          }
        }
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject, openSession, sessionName],
  );

  /** 把一条消息作为新一轮发给模型（回显 user + 起 assistant 流式占位）。
   *  `display` 是回显正文（保留 chip 占位符），`text` 是发给模型的纯文本。 */
  const dispatchTurn = useCallback(
    async (
      project: string,
      session: string,
      text: string,
      display: string,
      refs: FileReference[],
      sendPaths: string[],
    ) => {
      const key = viewKey(project, session);
      if (views[key]?.busy ?? false) return;
      const assistantId = nextId();
      turnRef.current.set(key, assistantId);
      turnStartRef.current.set(key, Date.now());
      setViews((prev) => {
        const view = prev[key] ?? emptyView();
        return {
          ...prev,
          [key]: {
            ...view,
            messages: [
              ...view.messages,
              { id: nextId(), role: "user", content: display, status: "complete", references: refs },
              { id: assistantId, role: "assistant", content: "", status: "streaming" },
            ],
            turnSegments: { ...view.turnSegments, [assistantId]: [] },
            busy: true,
            error: "",
          },
        };
      });

      try {
        const bridge = await agentBridge();
        await bridge.send(project, session, text, sendPaths);
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        applyEvent(key, { type: "error", message, project, session });
      }
    },
    [views, applyEvent],
  );

  const send = useCallback(async () => {
    const project = activeProject;
    const session = sessionName;
    if (!project || !session) return;
    const key = viewKey(project, session);
    if (views[key]?.busy ?? false) return;

    const refs = references;
    // `/` 技能入口（与 TUI 同语义）：技能以 chip 形式出现在输入框里；chip 之后的
    // 文本即 args。无参 = 只激活 + 本地系统提示（不发模型）；有参 = 激活 + 把 args
    // 作为本轮 prompt。desktop 无本地内置 slash 指令，只有 chip 才走这条路径。
    const skill = refs.find((reference) => reference.kind === "skill");
    if (skill) {
      try {
        await (await agentBridge()).activateSkill(project, session, skill.name);
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
        return;
      }
      slashSearch.close();
      setInput("");
      setReferences([]);
      setUiError("");
      const args = stripTokens(input).trim();
      if (!args) {
        // 无参：只激活，不打扰模型，给一条本地系统提示（保留 chip 回显）。
        setViews((prev) => {
          const view = prev[key] ?? emptyView();
          return {
            ...prev,
            [key]: {
              ...view,
              messages: [
                ...view.messages,
                {
                  id: nextId(),
                  role: "user",
                  content: `${CHIP_TOKEN} 已激活技能（本轮起每轮注入其指令）。`,
                  status: "complete",
                  references: [skill],
                },
              ],
            },
          };
        });
        return;
      }
      // 有参：激活后把 args 当本轮 prompt 继续走正常发送流程；display 保留 chip，
      // 但**不把技能名当引用路径发给后端**（sendPaths 为空）。
      await dispatchTurn(project, session, args, `${CHIP_TOKEN}${args}`, [skill], []);
      return;
    }

    // 发给模型 / 判空的正文要剥掉占位符（占位符不是用户文字）；但**回显用的
    // content 必须保留占位符**——`InlineReferences` 正是按占位符位置把 chip 落回
    // 正文的，剥掉就一个 tag 都渲染不出来（曾经的 bug）。
    const plain = stripTokens(input).trim();
    if (!plain) return;
    setInput("");
    setReferences([]);
    setUiError("");
    await dispatchTurn(
      project,
      session,
      plain,
      input.trim(),
      refs,
      refs.map((reference) => reference.path),
    );
  }, [activeProject, input, references, sessionName, views, slashSearch, dispatchTurn]);

  const stop = useCallback(async () => {
    const project = activeProject;
    const session = sessionName;
    if (!project || !session) return;
    // 只发取消信号：真正的收尾（回退本轮 + 把 `busy` 置 false）由后端回推的
    // `finished(cancelled)` 事件统一完成——避免这里先乐观置 `busy=false`，用户
    // 立刻重发时又与后端“agent 正在运行中”判定打架。
    await (await agentBridge()).cancel(project, session).catch(() => {});
  }, [activeProject, sessionName]);

  const switchModel = useCallback(
    async (value: string) => {
      if (!activeProject) return;
      try {
        const bridge = await agentBridge();
        await bridge.setModel(activeProject, value);
        setModel(value);
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject],
  );

  const view =
    activeProject && sessionName
      ? (views[viewKey(activeProject, sessionName)] ?? emptyView())
      : emptyView();
  const busy = view.busy;
  const error = uiError || view.error;

  return (
    <DesktopChrome
      title={(activeProject && sessionName && sessionTitles[viewKey(activeProject, sessionName)]) || (sessionName ? "新建会话" : "Shirley")}
      projectName={projects.find((entry) => entry.id === activeProject)?.name}
      sidebarCollapsed={sidebarCollapsed}
      canGoBack={navigationHistory.index > 0}
      canGoForward={navigationHistory.index < navigationHistory.entries.length - 1}
      navigationBusy={navigationBusy}
      onBack={() => void navigateHistory(-1)}
      onForward={() => void navigateHistory(1)}
      onToggleSidebar={toggleSidebar}
      onHome={() => {
        setSidebarCollapsed(false);
        writeBool(SIDEBAR_STORAGE_KEY, false);
      }}
      onNewChat={() => activeProject ? void newSession(activeProject) : void addProject()}
      onAddProject={() => void addProject()}
    >
      {/* 左侧 sidebar：NewChat / 置顶 / 项目（多项目 + 会话导航）。 */}
      <SessionSidebar
        collapsed={sidebarCollapsed}
        onCurrentTitleChange={updateCurrentTitle}
        projects={projects}
        activeProject={activeProject}
        current={sessionName}
        onAddProject={() => void addProject()}
        onRenameProject={(id, name) => void renameProject(id, name)}
        onArchiveProject={(id) => void archiveProject(id)}
        onSelect={(project, name) => void switchSession(project, name)}
        onNew={(project) => void newSession(project)}
        onRename={(project, name, title) => void renameSession(project, name, title)}
        onDelete={(project, name) => void deleteSession(project, name)}
      />
      {/* 右侧主区：固定宽度居中（对齐 shiwen AiConversationSurface 的 max-w-190 = 760px）。
          聊天区与输入框都不随窗口拉宽，长文本按阅读舒适宽度换行。 */}
      <main className="relative flex min-h-0 min-w-0 flex-1 flex-col">
        <section className="mx-auto flex w-full max-w-190 min-h-0 flex-1 flex-col">
        {view.loading ? (
          // 历史快照尚未到达：先渲染骨架屏，给用户「正在加载消息记录」的即时反馈。
          <div className="min-h-0 flex-1 overflow-hidden">
            <AiConversationSkeleton />
          </div>
        ) : (
        <AiConversationTranscript
          messages={view.messages}
          conversationKey={activeProject && sessionName ? viewKey(activeProject, sessionName) : "main"}
          viewportRef={transcriptViewportRef}
          extraContentKey={busy ? "busy" : ""}
          emptyContent={
            <div className="flex flex-col items-center gap-4 px-4 py-6 text-center sm:py-10">
              <img
                src="/brand/shirley-character.png"
                alt="《Code Geass》中的夏利·菲内特，橙色长发，身穿阿什弗德学园制服"
                width={768}
                height={1152}
                className="h-[clamp(140px,32vh,280px)] w-auto max-w-full object-contain"
                draggable={false}
              />
              <div className="space-y-2">
                <h1 className="font-display text-title-sm text-ink">Shirley</h1>
                <p className="text-body-sm text-muted">
                  开始和 Shirley 对话吧。输入 <kbd className="font-utility">@</kbd> 引用工作区文件，或 <kbd className="font-utility">/</kbd> 调用技能。
                </p>
              </div>
            </div>
          }
          renderMessageContent={(message) =>
            message.role === "assistant" ? (
              <AssistantMessage
                content={message.content}
                streaming={message.status === "streaming"}
                segments={view.turnSegments[String(message.id)] ?? []}
                durationMs={message.durationMs}
                failed={message.status === "error"}
                stopReason={message.stopReason}
              />
            ) : (
              <InlineReferences content={message.content} references={message.references ?? []} />
            )
          }
        />
        )}
        {error && (
          <p className="mx-3 mb-1 text-body-sm text-danger-ink" role="alert">
            {error}
          </p>
        )}
        {/* 任务账本：模型自维护的任务进度，常驻在输入框上方——「任务在推进」最直接的信号。 */}
        {view.todo && <TaskCard task={view.todo} />}
        {/* 运行态状态行：压缩中 / 上下文占用。让「过程」有可见反馈，而不是界面静默。 */}
        <SessionStatusLine
          compressing={view.compressing}
          contextUsage={view.contextUsage}
        />
        <div className="relative shrink-0 px-3 pb-3">
          {slashSearch.open && (
            <div className="absolute inset-x-3 bottom-full z-30 mb-1">
              <SkillSlashPopover
                open={slashSearch.open}
                query={slashSearch.query}
                results={slashSearch.results}
                error={slashSearch.error}
                selectedIndex={slashSearch.selectedIndex}
                onSelect={(result) => editorRef.current?.insertSkill(result)}
                onRetry={() => {}}
              />
            </div>
          )}
          {!slashSearch.open && mentionSearch.popoverOpen && (
            <div className="absolute inset-x-3 bottom-full z-30 mb-1">
              <FileMentionPopover
                open={mentionSearch.popoverOpen}
                query={mentionSearch.query}
                results={mentionSearch.results}
                loading={mentionSearch.loading}
                error={mentionSearch.error}
                selectedIndex={mentionSearch.selectedIndex}
                onSelect={(result) => editorRef.current?.insertReference(result)}
                onRetry={mentionSearch.retry}
                showSearch={false}
              />
            </div>
          )}
          <AiChatComposer
            id="shirley-composer"
            label="发送消息"
            value={stripTokens(input)}
            placeholder="给 Shirley 发消息…（@ 引用文件，/ 调用技能）"
            busy={busy}
            onValueChange={() => {}}
            onSend={() => void send()}
            onStop={() => void stop()}
            trailingAction={
              <ModelSelector project={activeProject ?? ""} current={model} onSelect={(value) => void switchModel(value)} />
            }
            inputSlot={
              <MentionEditor
                ref={editorRef}
                id="shirley-composer"
                label="发送消息"
                value={input}
                references={references}
                placeholder="给 Shirley 发消息…（@ 引用文件，/ 调用技能）"
                autoFocus
                search={mentionSearch}
                slash={slashSearch}
                onChange={(text, refs) => {
                  setInput(text);
                  setReferences(refs);
                }}
                onSubmit={() => void send()}
              />
            }
          />
        </div>
        </section>
        {/* 对话轨迹：挂在聊天区最外层容器（main）的左侧边缘，hover 预览历史用户轮次。 */}
        {view.messages.some((message) => message.role === "user") && (
          <AiConversationMinimap
            messages={view.messages}
            viewportRef={transcriptViewportRef}
            className="left-6"
          />
        )}
      </main>
    </DesktopChrome>
  );
}

/**
 * 把一个事件应用到视图（纯函数）。
 *
 * 合并规则对标 TUI `append_streaming_delta`：连续同类型的增量并进同一段，类型一换
 * 就新开一段——渲染顺序天然是 思考 → 正文 → 工具 → 正文 → 思考 → 工具 ……。
 */
function applyWireEvent(
  view: SessionView,
  event: AgentEventWire,
  assistantId: string | null,
): SessionView {
  switch (event.type) {
    case "compact": {
      if (!assistantId) return view;
      return {
        ...view,
        turnSegments: appendSegment(view.turnSegments, assistantId, {
          kind: "compact", text: event.content,
        }),
      };
    }
    case "content_delta": {
      if (!assistantId) return view;
      return {
        ...view,
        messages: view.messages.map((message) =>
          message.id === assistantId
            ? { ...message, content: message.content + event.text, status: "streaming" }
            : message,
        ),
        turnSegments: appendSegment(view.turnSegments, assistantId, {
          kind: "content",
          text: event.text,
        }),
      };
    }
    case "tool_started": {
      if (!assistantId) return view;
      return {
        ...view,
        turnSegments: appendSegment(view.turnSegments, assistantId, {
          kind: "tools",
          executions: [
            {
              id: event.call_id,
              call_id: event.call_id,
              tool_name: event.name,
              status: "running",
              summary: "",
              input: safeParseArgs(event.arguments),
            },
          ],
        }),
      };
    }
    case "tool_finished": {
      if (!assistantId) return view;
      return {
        ...view,
        turnSegments: updateTool(view.turnSegments, assistantId, event),
      };
    }
    case "todo_updated": {
      // 任务账本变更：只更新结构化视图，不进消息流——任务卡是独立于 transcript 的
      // 「当前进度」指示，模型每次调用 `todo` 工具都会刷新它。
      return { ...view, todo: event.todo };
    }
    case "compression_started": {
      // 压缩期间给状态行一个明确信号（对标 TUI 的「正在压缩上下文」）。
      return { ...view, compressing: true };
    }
    case "compression_finished": {
      return { ...view, compressing: false };
    }
    case "usage": {
      return { ...view, usage: event.usage };
    }
    case "context_usage": {
      return {
        ...view,
        contextUsage: { used: event.used_tokens, limit: event.limit_tokens },
      };
    }
    case "error": {
      return {
        ...view,
        error: event.message,
        busy: false,
        compressing: false,
        messages: view.messages.map((message) =>
          message.id === assistantId ? { ...message, status: "error" } : message,
        ),
      };
    }
    case "finished": {
      return {
        ...view,
        busy: false,
        compressing: false,
        messages: view.messages.map((message) =>
          message.id === assistantId ? { ...message, status: "complete", stopReason: event.stop_reason } : message,
        ),
      };
    }
    default:
      return view;
  }
}

function appendSegment(
  segments: Record<string, AiSegment[]>,
  assistantId: string,
  segment: AiSegment,
): Record<string, AiSegment[]> {
  const current = segments[assistantId] ?? [];
  const last = current.at(-1);
  if (segment.kind === "content" && last?.kind === "content") {
    return {
      ...segments,
      [assistantId]: [...current.slice(0, -1), { ...last, text: last.text + segment.text }],
    };
  }
  if (segment.kind === "tools" && last?.kind === "tools") {
    return {
      ...segments,
      [assistantId]: [
        ...current.slice(0, -1),
        { ...last, executions: [...last.executions, ...segment.executions] },
      ],
    };
  }
  return { ...segments, [assistantId]: [...current, segment] };
}

function updateTool(
  segments: Record<string, AiSegment[]>,
  assistantId: string,
  event: Extract<AgentEventWire, { type: "tool_finished" }>,
): Record<string, AiSegment[]> {
  const current = segments[assistantId] ?? [];
  const next = [...current];
  let fallback: { segmentIndex: number; executionIndex: number } | null = null;
  for (let segmentIndex = next.length - 1; segmentIndex >= 0; segmentIndex -= 1) {
    const segment = next[segmentIndex];
    if (segment.kind !== "tools") continue;
    let executionIndex = segment.executions.length - 1;
    while (executionIndex >= 0 && segment.executions[executionIndex].call_id !== event.call_id) {
      executionIndex -= 1;
    }
    if (executionIndex < 0) continue;
    if (segment.executions[executionIndex].status !== "running") {
      fallback ??= { segmentIndex, executionIndex };
      continue;
    }
    fallback = { segmentIndex, executionIndex };
    break;
  }
  if (fallback) {
    const { segmentIndex, executionIndex } = fallback;
    const segment = next[segmentIndex];
    if (segment.kind !== "tools") return segments;
    const executions = [...segment.executions];
    executions[executionIndex] = {
      ...executions[executionIndex],
      status: event.ok ? "complete" : "error",
      summary: event.output.slice(0, 200),
      result_json: event.output,
      error: event.ok ? null : event.output,
    };
    next[segmentIndex] = { ...segment, executions };
  }
  return {
    ...segments,
    [assistantId]: next,
  };
}

/**
 * 从会话快照重建视图（订阅一个已积累历史的会话时用）。
 *
 * 快照是 `ItemWire` 序列（message / tools）；两条 user 消息之间的所有 assistant /
 * 工具条目归并成**同一轮** assistant（与实时流的"一轮一个 assistantId"口径一致）。
 * 返回视图 + 最后一个 assistant 轮次 id（供后续增量落点）。
 */
function viewFromSnapshot(snapshot: SessionSnapshot): {
  view: SessionView;
  lastAssistantId: string | null;
} {
  const messages: AiTranscriptMessage[] = [];
  const turnSegments: Record<string, AiSegment[]> = {};
  let currentAssistantId: string | null = null;

  const ensureAssistant = (): string => {
    if (!currentAssistantId) {
      const id = nextId();
      currentAssistantId = id;
      messages.push({ id, role: "assistant", content: "", status: "complete" });
      turnSegments[id] = [];
    }
    return currentAssistantId;
  };

  // 保序 + 相邻同类合并：连续 content / tools 段并成一段，与实时流
  // （appendSegment）同一口径。跨类不合并，时间顺序不变。
  const pushSegment = (id: string, segment: AiSegment): void => {
    const list = turnSegments[id];
    const last = list.at(-1);
    if (segment.kind === "content" && last?.kind === "content") {
      list[list.length - 1] = { ...last, text: last.text + segment.text };
      return;
    }
    if (segment.kind === "tools" && last?.kind === "tools") {
      list[list.length - 1] = { ...last, executions: [...last.executions, ...segment.executions] };
      return;
    }
    list.push(segment);
  };

  for (const item of snapshot.items) {
    if (item.kind === "compact" || (item.kind === "message" && item.role === "summary")) {
      // 与实时 compact 事件保持相同投影；兼容旧 summary 快照。
      const id = ensureAssistant();
      pushSegment(id, { kind: "compact", text: item.content });
      continue;
    }
    if (item.kind === "message") {
      if (item.role === "user") {
        messages.push({ id: nextId(), role: "user", content: item.content, status: "complete" });
        currentAssistantId = null;
        continue;
      }
      // assistant / system / error 落到 assistant 轮次。
      const id = ensureAssistant();
      pushSegment(id, { kind: "content", text: item.content });
      const message = messages.find((m) => m.id === id);
      if (message) message.content += item.content;
      if (message && item.role === "error") message.status = "error";
    } else {
      const id = ensureAssistant();
      pushSegment(id, {
        kind: "tools",
        executions: item.calls.map(
          (call): AiToolExecution => ({
            id: call.id,
            call_id: call.id,
            tool_name: call.name,
            status: "complete",
            summary: "",
            input: safeParseArgs(call.arguments),
          }),
        ),
      });
    }
  }

  if (snapshot.running) {
    const id = ensureAssistant();
    const current = messages.find((message) => message.id === id);
    if (current) current.status = "streaming";
  }

  return {
    view: {
      messages,
      turnSegments,
      busy: snapshot.running,
      error: "",
      loading: false,
      todo: snapshot.todo,
      compressing: false,
      contextUsage: null,
      usage: null,
    },
    lastAssistantId: currentAssistantId,
  };
}

function safeParseArgs(args: string): Record<string, unknown> | undefined {
  try {
    const parsed = JSON.parse(args);
    return typeof parsed === "object" && parsed ? parsed : undefined;
  } catch {
    return undefined;
  }
}
