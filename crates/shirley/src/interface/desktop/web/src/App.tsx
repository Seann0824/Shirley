import { useCallback, useEffect, useRef, useState } from "react";
import { Clock, ImagePlus, ListChecks, LoaderCircle, Plus, X } from "lucide-react";
import { DesktopChrome } from "@/components/DesktopChrome";
import {
  AiConversationTranscript,
  type AiTranscriptMessage,
} from "@/components/ai/AiConversationTranscript";
import { AiChatComposer } from "@/components/ai/AiChatComposer";
import { Button } from "@/ui/button";
import { AssistantMessage } from "@/components/AssistantMessage";
import { ImageHoverPreview } from "@/components/ImageHoverPreview";
import { TaskCard } from "@/components/ai/TaskCard";
import { ModelSelector } from "@/components/ai/ModelSelector";
import { SessionSidebar } from "@/components/ai/SessionSidebar";
import { EditProjectDialog } from "@/components/ai/EditProjectDialog";
import { WorkspaceTabs, type WebTab } from "@/components/ai/WorkspaceTabs";
import { HeaderTabs } from "@/components/HeaderTabs";
import type { BrowserTabMeta } from "@/components/ai/BrowserView";
import { browserBridge } from "@/lib/browser-bridge";
import { NewChatControls } from "@/components/ai/NewChatControls";
import { SubagentSection } from "@/components/ai/SubagentSection";
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
import {
  agentBridge,
  projectSource,
  splitProject,
  tagProject,
  type DataSource,
  type ProjectEntry,
  type SessionSubscription,
} from "@/lib/bridge";
import type { AgentEventWire, SessionSnapshot, StateSnapshotWire, TaskViewWire, UsageWire } from "@/types/wire";
import type { AiSegment, AiToolExecution } from "@/types/ai";
import { cn } from "@/ui/utils";
import { MOBILE_LAYOUT_QUERY, useMobileLayout } from "@/lib/use-mobile-layout";

let messageSeq = 0;
const nextId = () => `m${++messageSeq}`;

/** Index of a message by id; falls back to the end (append) when not found. */
function indexOfMessage(messages: AiTranscriptMessage[], id: string | undefined): number {
  if (id == null) return messages.length;
  const index = messages.findIndex((message) => String(message.id) === String(id));
  return index < 0 ? messages.length : index;
}

/** Sidebar collapse preference (local UI state only; not synced / persisted). */
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
    // Silently degrade when localStorage is unavailable (private mode, etc.): state is valid for this session only.
  }
}

/** Auto-collapse the sidebar when the viewport is narrower than this (256px sidebar + 760px content no longer fits). */
const SIDEBAR_AUTO_COLLAPSE_PX = 900;

/** Fixed chat-area width (`max-w-190` = 190 x 4px = 760px), used to compute the left gutter available for the minimap. */
const CHAT_MAX_WIDTH_PX = 760;
/** Minimap position: 12px from the `main` left edge (`left-3`). */
const MINIMAP_LEFT_PX = 12;
/** Minimap width: 40px (`w-10`). */
const MINIMAP_WIDTH_PX = 40;

/**
 * Pseudo project id for Chat mode (aligned with Rust `shell.rs::CHAT_PROJECT_ID`).
 *
 * Chat mode is **not associated with any user project**, but reuses the same
 * project-scoped commands: the backend routes this id to the runtime under
 * `<config_dir>/shirley/chat`, so the frontend needs no separate contract for Chat —
 * session / model / file / skill commands just pass `project = __chat__` as usual.
 */
const CHAT_PROJECT_ID = "__chat__";

/**
 * UI mode: Chat (no associated project) / Work (switchable associated project). **Derived
 * from the foreground project**; not persisted separately.
 */
type AppMode = "chat" | "work";

/**
 * A "pending" message: submitted early by the user while a turn is running, queued to
 * send automatically once the previous turn ends.
 *
 * Corresponds one-to-one with `dispatchTurn`'s params (`display` for echo, `text` sent
 * to the model, `refs` / `sendPaths` for `@` references). Stored per session in
 * `SessionView.pending`; not lost when switching away and back.
 */
type PendingTurn = {
  id: string;
  project: string;
  session: string;
  text: string;
  display: string;
  refs: FileReference[];
  sendPaths: string[];
  /** Pasted images as `data:` URLs (routed to `agent_send`'s `images` channel). */
  images: string[];
};

/**
 * View state for a single session (transcript + per-turn segments + busy / error).
 *
 * Multi-project multi-session: events are tagged with `project` + `session`; each
 * session holds its own view; the current session renders while background sessions keep
 * accumulating (context is not lost when switching away). Switching sessions = changing
 * which view renders + which event stream is subscribed. The view key is
 * `project::session` — session names are unique only within a project, so they collide
 * across projects.
 */
type SessionView = {
  messages: AiTranscriptMessage[];
  turnSegments: Record<string, AiSegment[]>;
  busy: boolean;
  retryStatus: string | null;
  error: string;
  /** History snapshot not yet arrived: render optimistically, showing a skeleton. */
  loading: boolean;
  /** Task-ledger snapshot (`null` = empty ledger / no ledger). Drives the task card and status line. */
  todo: TaskViewWire | null;
  /** Compressing context (`compression_started` -> `compression_finished`). */
  compressing: boolean;
  /** Context usage (used / limit tokens), from the `context_usage` event. */
  contextUsage: { used: number; limit: number } | null;
  /** Most recently reported usage, from the `usage` event. */
  usage: UsageWire | null;
  /** Pending queue: messages the user submitted early while busy, sent automatically in order once the previous turn ends. */
  pending: PendingTurn[];
};

function emptyView(): SessionView {
  return {
    messages: [],
    turnSegments: {},
    busy: false,
    retryStatus: null,
    error: "",
    loading: false,
    todo: null,
    compressing: false,
    contextUsage: null,
    usage: null,
    pending: [],
  };
}

/** View key: unique across projects. */
const viewKey = (project: string, session: string) => `${project}::${session}`;

type SessionLocation = { project: string; session: string };
type NavigationHistory = { entries: SessionLocation[]; index: number };

function removeLocations(history: NavigationHistory, remove: (entry: SessionLocation) => boolean): NavigationHistory {
  const entries = history.entries.filter((entry) => !remove(entry));
  const index = history.entries.slice(0, history.index + 1).filter((entry) => !remove(entry)).length - 1;
  return { entries, index };
}

/**
 * Content-free runtime-status pill (P2): derived purely from the `status://event` stream.
 *
 * Unlike the render stream (which only fires on an `AgentEvent`), the status channel ticks
 * every second even while the model is streaming or a tool is running — so it can tell
 * "long generation" apart from "actually stuck". It carries no prompt / output / tool args.
 *
 * `Worked for Ns` is derived from the backend's `seconds_since_turn_start`; the stall hint
 * from `seconds_since_progress` (last *real* progress: a completed model or tool call).
 */
function StatusPill({
  status,
  compressing,
  contextUsage,
}: {
  status: StateSnapshotWire | null;
  compressing: boolean;
  contextUsage: { used: number; limit: number } | null;
}) {
  const percent =
    contextUsage && contextUsage.limit > 0
      ? Math.round((contextUsage.used / contextUsage.limit) * 100)
      : null;
  const idle = !status || status.liveness === "idle";
  // Nothing to show at all: no live status, no compression, no context usage.
  if (idle && !compressing && percent === null) return null;

  const seconds = status?.seconds_since_turn_start ?? null;
  const stalled = status ? status.liveness === "stalled" || status.liveness === "unreachable" : false;
  const tone = !status
    ? "text-muted"
    : status.stale || status.liveness === "unreachable"
      ? "text-danger-ink"
      : stalled
        ? "text-warn-ink"
        : "text-muted";
  const label = !status || status.liveness === "idle"
    ? null
    : status.stale
      ? "stale · last known state"
      : status.liveness === "unreachable"
        ? "connection lost"
        : status.liveness === "stalled"
          ? `stalled · no progress for ${status.seconds_since_progress ?? 0}s`
          : status.health === "compacting"
            ? "compacting context"
            : status.health === "retrying"
              ? "retrying"
              : status.health === "slow_tool"
                ? `slow tool${status.health_detail ? ` · ${status.health_detail}` : ""}`
                : status.activity === "tool_call"
                  ? "running tool"
                  : status.activity === "model_call"
                    ? "thinking"
                    : "working";
  const spinning = compressing || (status ? !stalled && status.liveness !== "idle" : false);
  return (
    <div className={cn("mx-3 mb-1 flex items-center gap-2 text-caption", tone)} role="status" aria-live="polite">
      {spinning && (
        <LoaderCircle className="size-3.5 animate-spin" aria-hidden="true" />
      )}
      {label && <span>{label}</span>}
      {percent !== null && (
        <span className="font-utility">Context {percent}%</span>
      )}
      {seconds !== null && !stalled && (
        <span className="font-utility">· worked for {seconds}s</span>
      )}
    </div>
  );
}

/**
 * Pending queue: messages the user submitted early while a turn was running, queued to
 * send automatically once the previous turn ends.
 *
 * Only appears when the foreground session has queued items; shows each body (with chip
 * placeholders stripped) and allows removal. Send order = enqueue order (first
 * submitted, first sent).
 */
function PendingQueue({ turns, onRemove }: { turns: PendingTurn[]; onRemove: (id: string) => void }) {
  if (turns.length === 0) return null;
  return (
    <div className="mx-3 mb-1 rounded-card border border-line bg-surface px-3 py-2 shadow-card">
      <div className="mb-1 flex items-center gap-1.5 text-caption text-muted">
        <Clock className="size-3.5" aria-hidden="true" />
        Pending · {turns.length}
      </div>
      <ul className="flex flex-col gap-1">
        {turns.map((turn) => (
          <li key={turn.id} className="flex items-start gap-2 text-body-sm text-ink">
            <span className="min-w-0 flex-1 truncate">{stripTokens(turn.display)}</span>
            <button
              type="button"
              aria-label="Remove this pending message"
              className="mt-0.5 shrink-0 rounded-control p-0.5 text-muted transition-colors hover:bg-selected hover:text-ink"
              onClick={() => onRemove(turn.id)}
            >
              <X className="size-3.5" />
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function App() {
  // One view per session (background sessions are kept too, so switching back does not lose them), keyed by `project::session`.
  const [views, setViews] = useState<Record<string, SessionView>>({});
  // Project list (active) and current project.
  const [projects, setProjects] = useState<ProjectEntry[]>([]);
  // Project being edited in the "Edit project" dialog (rename + source folders); null = closed.
  const [editingProjectEntry, setEditingProjectEntry] = useState<ProjectEntry | null>(null);
  const [activeProject, setActiveProject] = useState<string | null>(null);
  const [sessionName, setSessionName] = useState<string | null>(null);
  // Which runtime a NEW send / new chat targets. Existing projects & sessions keep their own
  // source (encoded in the namespaced project id `local::<id>` / `cloud::<id>`); this only seeds
  // the composer's Local/Cloud choice and which `__chat__` pseudo project is foreground.
  const [preferredSource, setPreferredSource] = useState<DataSource>("local");
  // Sources the hybrid bridge can reach (cloud is best-effort; may be local-only).
  const [availableSources, setAvailableSources] = useState<DataSource[]>(["local"]);
  const [input, setInput] = useState("");
  const [references, setReferences] = useState<FileReference[]>([]);
  // "Edit the last user message and resend" mode: which session view + which message id
  // is being edited. Only the last user message is ever editable (option A, same as TUI).
  const [editingTurn, setEditingTurn] = useState<{ key: string; messageId: string } | null>(null);
  const [model, setModel] = useState("");
  // UI-level error (project / session / model switch failure); runtime errors go into each session view's `error`.
  const [uiError, setUiError] = useState("");
  const [navigationHistory, setNavigationHistory] = useState<NavigationHistory>({ entries: [], index: -1 });
  const [navigationBusy, setNavigationBusy] = useState(false);
  const [sessionTitles, setSessionTitles] = useState<Record<string, string>>({});
  // Content-free runtime status per session (`project::session`), from the separate
  // `status://event` channel. Kept apart from `views` because it ticks independently of
  // the render stream and carries no content (see docs/agent-status.md §6.1 / P2).
  const [statuses, setStatuses] = useState<Record<string, StateSnapshotWire>>({});
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
  // Sidebar collapse: user's manual preference (persisted). Auto-collapsed when the window is too narrow; can still be manually expanded afterward.
  const mobile = useMobileLayout();
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() =>
    window.matchMedia(MOBILE_LAYOUT_QUERY).matches || readBool(SIDEBAR_STORAGE_KEY, false),
  );
  // Top-bar tabs follow the **session** (Codex desktop model): each session owns its own
  // independent tab space — its chat tab plus the web pages it has opened. Switching the
  // session (from the sidebar) swaps the whole strip; tabs never leak across sessions.
  // `activeWebTabId === null` means the session's chat tab is foreground, otherwise that
  // web tab is. Both maps are keyed by `project::session`.
  const browserAvailable = browserBridge.available;
  const [sessionWebTabs, setSessionWebTabs] = useState<Record<string, WebTab[]>>({});
  const [sessionActiveWebTab, setSessionActiveWebTab] = useState<Record<string, string | null>>({});
  const webTabSeqRef = useRef(0);
  // Foreground session key (null when nothing is open yet). All tab reads derive from it.
  const currentKey = activeProject && sessionName ? viewKey(activeProject, sessionName) : null;
  const webTabs = currentKey ? (sessionWebTabs[currentKey] ?? []) : [];
  const activeWebTabId = currentKey ? (sessionActiveWebTab[currentKey] ?? null) : null;
  // Every session's web tabs, flattened, so `WorkspaceTabs` keeps all of their native
  // `BrowserView`s mounted (unmounting a tab would orphan its Rust child webview — it
  // would not be hidden/destroyed and could float over the chat). Only the current
  // session's active tab is actually visible.
  const allWebTabs = Object.values(sessionWebTabs).flat();
  // Task ledger panel: **not resident** — the user opens it from the composer toggle.
  // Resets to closed when switching sessions (keyed by `project::session`), so a new
  // session never inherits a stale "open" state.
  const [taskPanelKey, setTaskPanelKey] = useState<string | null>(null);
  // Hidden `<input type="file">` for picking image files (the paste path is not enough:
  // users also want to choose an image from disk).
  const imageInputRef = useRef<HTMLInputElement>(null);
  // UI mode is derived from "whether the foreground is attached to a project": pseudo
  // project `__chat__` => Chat (no associated project), otherwise => Work. There is no
  // separate mode state — switching projects / switching to Chat switches the mode,
  // avoiding two sources of truth.
  const isChatProject = (id: string | null): boolean =>
    id != null && splitProject(id).rawId === CHAT_PROJECT_ID;
  const mode: AppMode = isChatProject(activeProject) ? "chat" : "work";
  // Chat pseudo project for the preferred source (both runtimes expose `__chat__`; the
  // namespaced id keeps the local and cloud ones distinct in one merged view).
  const chatProjectId = tagProject(preferredSource, CHAT_PROJECT_ID);
  // Source of the foreground context (drives the composer toggle + source badges).
  const activeSource: DataSource = activeProject ? projectSource(activeProject) : preferredSource;

  const closeSidebar = useCallback(() => setSidebarCollapsed(true), []);
  const toggleSidebar = useCallback(() => {
    setSidebarCollapsed((prev) => {
      const next = !prev;
      if (!mobile) writeBool(SIDEBAR_STORAGE_KEY, next);
      return next;
    });
  }, [mobile]);

  // Auto-collapse when the window cannot fit: folds as soon as a narrow viewport is entered (does not auto-expand, respecting the user's later explicit choice).
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
  // The minimap attaches to the left edge of the chat area's outermost container (main) and needs to share the scroll viewport.
  const transcriptViewportRef = useRef<HTMLDivElement>(null);
  // The minimap shows only when the left gutter fits it — otherwise it is hidden outright, never overlapping the chat content.
  const mainRef = useRef<HTMLElement>(null);
  const [minimapFits, setMinimapFits] = useState(false);
  useEffect(() => {
    const main = mainRef.current;
    if (!main) return;
    main.inert = mobile && !sidebarCollapsed;
    return () => { main.inert = false; };
  }, [mobile, sidebarCollapsed]);
  useEffect(() => {
    if (mobile) setSidebarCollapsed(true);
  }, [mobile]);
  useEffect(() => {
    const el = mainRef.current;
    if (!el) return;
    const measure = () => {
      // The section is centered and max 760px: left gutter = (main width - 760) / 2 (0 when main width is under 760).
      const gutter = Math.max(0, (el.clientWidth - CHAT_MAX_WIDTH_PX) / 2);
      setMinimapFits(gutter >= MINIMAP_LEFT_PX + MINIMAP_WIDTH_PX);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  // Current subscription handle (unsubscribed when switching away).
  const subRef = useRef<SessionSubscription | null>(null);
  const openEpochRef = useRef(0);
  // The assistant turn id currently running for each session (events land on it), keyed by `project::session`.
  const turnRef = useRef<Map<string, string>>(new Map());
  // Start timestamp of this turn for each session (to compute "Worked for Ns"), keyed by `project::session`.
  const turnStartRef = useRef<Map<string, number>>(new Map());
  // Buffer events before the snapshot is applied (deltas arriving between subscribe and snapshot are not lost).
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

  // Subscribe to the content-free runtime-status stream once for the app's lifetime.
  // Snapshots arrive tagged with `project` + `session`; store them per `project::session`.
  // Kept separate from the render stream: it ticks every second even without an
  // `AgentEvent` (that is what lets the UI tell "long generation" from "actually stuck").
  useEffect(() => {
    let unsubscribe: (() => void) | null = null;
    let disposed = false;
    void (async () => {
      const bridge = await agentBridge();
      const stop = await bridge.subscribeStatus((snapshot) => {
        if (snapshot.project == null || snapshot.session == null) return;
        const key = viewKey(snapshot.project, snapshot.session);
        setStatuses((prev) => ({ ...prev, [key]: snapshot }));
      });
      if (disposed) stop();
      else unsubscribe = stop;
    })().catch(() => {});
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, []);

  // Apply an event to the given session's view (functional update to avoid stale closures).
  const applyEvent = useCallback((key: string, event: AgentEventWire) => {
    // User clicked stop: the backend **keeps** this turn (user message + half-finished
    // streaming reply) for the next turn to reference, instead of rolling back and
    // deleting them (requirement 2), and finishes with `finished(cancelled)`. Here we
    // only mark this turn's assistant from streaming to complete and clear `busy` —
    // **without discarding entries or rolling back input** (matching the TUI
    // `App::complete_turn` interruption semantics).
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
                  ...execution, status: "error" as const, error: "Interrupted; execution status needs confirmation",
                } : execution),
              } : segment),
            ])),
            busy: false,
            retryStatus: null,
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
      // Turn finished (complete / error): write this turn's duration onto the assistant message for the "Worked for Ns" disclosure.
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

  // Respond to interaction first: immediately switch the foreground session and enter
  // the loading state (render a skeleton), without waiting for the backend round trip.
  // Both opening and switching sessions call this **before** fetching the history
  // snapshot — the snapshot needs an `agent_subscribe` round trip, and the user should
  // not be left staring at the old session.
  const beginSessionLoad = useCallback((project: string, session: string, silent = false) => {
    const key = viewKey(project, session);
    setSessionName(session);
    setViews((prev) => {
      const existing = prev[key];
      // Cached content already exists (a background session has run): reuse it without flashing a skeleton; otherwise show a loading placeholder.
      // `silent`: a newly created empty session is known to be empty (lazy session), so
      // no skeleton is needed — it settles into an empty view directly, with no loading
      // flicker when switching Chat/Work.
      const loading = !silent;
      if (existing && existing.messages.length > 0) {
        return { ...prev, [key]: { ...existing, loading } };
      }
      // The pending queue is pure frontend state and is not in the backend snapshot — it
      // must be preserved as-is when rebuilding the view, otherwise switching away and
      // back (or reopening the session) would clear it.
      return { ...prev, [key]: { ...emptyView(), loading, pending: existing?.pending ?? [] } };
    });
  }, []);

  // Subscribe to a session: unsubscribe the old one, rebuild the view from the snapshot, then layer on subsequent events.
  const openSession = useCallback(
    async (project: string, session: string, silent = false) => {
      const epoch = ++openEpochRef.current;
      const key = viewKey(project, session);
      beginSessionLoad(project, session, silent);
      const bridge = await agentBridge().catch((error) => {
        if (epoch === openEpochRef.current) setViews((prev) => ({
          ...prev, [key]: { ...(prev[key] ?? emptyView()), loading: false, error: String(error) },
        }));
        throw error;
      });
      if (epoch !== openEpochRef.current) return;
      subRef.current?.unsubscribe();
      subRef.current = null;
      readyRef.current.delete(key);
      bufferRef.current.delete(key);
      turnRef.current.delete(key);
      let sub;
      try {
        sub = await bridge.openSession(project, session, (event) => {
          // Snapshot not yet applied: buffer first, then replay in order once it lands (nothing lost or duplicated).
          if (!readyRef.current.has(key)) {
            const buffer = bufferRef.current.get(key) ?? [];
            buffer.push(event);
            bufferRef.current.set(key, buffer);
            return;
          }
          applyEvent(key, event);
        });
      } catch (e) {
        if (epoch !== openEpochRef.current) return;
        // Subscription failed: exit the loading state and report the error truthfully, instead of letting the skeleton spin forever.
        const message = e instanceof Error ? e.message : String(e);
        setViews((prev) => ({
          ...prev,
          [key]: { ...(prev[key] ?? emptyView()), loading: false, error: message },
        }));
        return;
      }
      if (epoch !== openEpochRef.current) { sub.unsubscribe(); return; }
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
            // The snapshot does not include the pending queue (the backend does not know about it); keep the locally queued items.
            pending: cached?.pending ?? [],
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

  // Select a project: switch foreground + open its current session.
  useEffect(() => {
    const recover = (event: Event) => {
      if ((event as CustomEvent<boolean>).detail && activeProject && sessionName) {
        void openSession(activeProject, sessionName);
      }
    };
    window.addEventListener("shirley-remote-connection", recover);
    return () => window.removeEventListener("shirley-remote-connection", recover);
  }, [activeProject, sessionName, openSession]);

  const selectProject = useCallback(
    async (project: string) => {
      try {
        setUiError("");
        if (project === activeProject) return;
        const epoch = ++openEpochRef.current;
        const bridge = await agentBridge();
        if (epoch !== openEpochRef.current) return;
        subRef.current?.unsubscribe();
        subRef.current = null;
        setSessionName(null);
        setActiveProject(project);
        const model = await bridge.modelName(project).catch(() => "");
        if (epoch !== openEpochRef.current) return;
        setModel(model);
        const name = await bridge.currentSession(project).catch(() => null);
        if (epoch !== openEpochRef.current) return;
        if (name) await openSession(project, name).catch(() => {});
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject, openSession],
  );

  // Startup: load the project list, open the most recently opened project (Work). Chat
  // is not persistent — clicking Chat in the top bar enters it (reusing the same
  // project-scoped commands, project = pseudo project `__chat__`). Chat sessions still
  // show up as the sidebar's "recent" list loads (independent of whether Chat mode is active).
  useEffect(() => {
    void (async () => {
      const bridge = await agentBridge();
      setAvailableSources(bridge.capabilities().sources ?? ["local"]);
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
      const caps = bridge.capabilities();
      let entry: ProjectEntry | null;
      if (caps.nativeFolderPicker) {
        entry = await bridge.addProject();
      } else {
        const path = window.prompt("Enter the absolute path of the project directory on the server:");
        if (!path) return; // user cancelled
        entry = await bridge.addProjectByPath(path.trim());
      }
      if (!entry) return; // user cancelled / failed
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
        // The archived project is the current one: clear the foreground and switch to the next available project (possibly none).
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

  // Save the "Edit project" dialog: rename + replace source folders (one of them primary).
  // If the primary folder changed, the backend rebuilds the runtime and the project's sessions
  // move to the new folder; we reload the project list so the sidebar reflects the new state.
  const updateProject = useCallback(
    async (project: string, changes: { name: string; primary: string; folders: string[] }) => {
      const bridge = await agentBridge();
      const updated = await bridge.updateProject(project, changes);
      await reloadProjects();
      // The active project's working dir may have moved: reopen its current session so the
      // conversation / tool context reflects the new primary folder.
      if (updated.id === activeProject) {
        setActiveProject(updated.id);
      }
    },
    [activeProject, reloadProjects],
  );

  // "Remove local project" = archive (no on-disk data is deleted), reusing the archive flow.
  const removeProjectFromDialog = useCallback(
    async (project: string) => {
      setEditingProjectEntry(null);
      await archiveProject(project);
    },
    [archiveProject],
  );

  const switchSession = useCallback(
    async (project: string, name: string) => {
      try {
        setUiError("");
        // Jump straight to the target session across projects, avoiding opening that project's old session midway and polluting the navigation history.
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
      // Hand it to the backend to find: a lazy session with no messages sent lives only in memory and won't appear in the disk list.
      // Only move the history cursor after a successful switch; on failure stay in the original session and allow retry.
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

  // Create a session under the given project (the "+" on a project row can act on any project, not just the foreground).
  const newSession = useCallback(
    async (project: string) => {
      try {
        setUiError("");
        const bridge = await agentBridge();
        // No title: the name is auto-generated by the AI after the first message; the user can rename it manually if desired.
        const entry = await bridge.newSession(project, null);
        setActiveProject(project);
        // silent: a newly created empty session is known to be empty; skip the skeleton (no loading flicker when switching Chat/Work).
        beginSessionLoad(project, entry.name, true);
        await openSession(project, entry.name, true);
        await bridge.modelName(project).then(setModel);
        // Focus the input directly — the intent of creating a session is "start chatting now", not managing first.
        editorRef.current?.focus();
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [openSession, beginSessionLoad],
  );

  // Web tabs (in-app browser pages) live in the **current session's** tab space.
  // Opening one foregrounds it; closing the active one falls back to its neighbor
  // (or the session's chat when none remain). Each tab keeps its own native BrowserView.
  const openWebTab = useCallback((initialUrl?: string) => {
    if (!currentKey) return;
    const key = currentKey;
    const id = `web-${++webTabSeqRef.current}`;
    setSessionWebTabs((prev) => ({ ...prev, [key]: [...(prev[key] ?? []), { id, initialUrl }] }));
    setSessionActiveWebTab((prev) => ({ ...prev, [key]: id }));
  }, [currentKey]);

  const closeWebTab = useCallback(
    (id: string) => {
      if (!currentKey) return;
      const key = currentKey;
      const tabs = sessionWebTabs[key] ?? [];
      const index = tabs.findIndex((tab) => tab.id === id);
      const next = tabs.filter((tab) => tab.id !== id);
      setSessionWebTabs((prev) => ({ ...prev, [key]: next }));
      if (activeWebTabId === id) {
        setSessionActiveWebTab((prev) => ({
          ...prev,
          [key]: next.length ? next[Math.min(index, next.length - 1)].id : null,
        }));
      }
      void browserBridge.close(id).catch(() => {});
    },
    [currentKey, sessionWebTabs, activeWebTabId],
  );

  // Every session's BrowserViews stay mounted, so a meta push must be routed to
  // whichever session owns that tab id (ids are globally unique).
  const onWebTabMeta = useCallback((meta: BrowserTabMeta) => {
    setSessionWebTabs((prev) => {
      let changed = false;
      const next: Record<string, WebTab[]> = {};
      for (const [key, tabs] of Object.entries(prev)) {
        const mapped = tabs.map((tab) => {
          if (tab.id !== meta.tabId) return tab;
          changed = true;
          return { ...tab, meta };
        });
        next[key] = mapped;
      }
      return changed ? next : prev;
    });
  }, []);

  // Switching Chat / Work within the new-chat flow: create a lazy empty session in the
  // target context (the Chat pseudo project or the first project), thereby switching the
  // mode along with it. When Work and there is no project, trigger "new project" instead.
  const switchNewChatMode = useCallback(
    async (next: AppMode) => {
      if (next === mode) return;
      if (next === "chat") {
        await newSession(tagProject(preferredSource, CHAT_PROJECT_ID));
        return;
      }
      const first =
        projects.find((entry) => projectSource(entry.id) === preferredSource)?.id ?? projects[0]?.id;
      if (!first) {
        await addProject();
        return;
      }
      await newSession(first);
    },
    [mode, projects, newSession, addProject, preferredSource],
  );

  // Switch the send target between local and cloud. Chat mode jumps to that source's
  // `__chat__`; Work mode jumps to the same-path sibling project when the other runtime has it.
  const switchSource = useCallback(
    async (next: DataSource) => {
      setPreferredSource(next);
      if (!availableSources.includes(next) || !activeProject) return;
      if (splitProject(activeProject).rawId === CHAT_PROJECT_ID) {
        await newSession(tagProject(next, CHAT_PROJECT_ID));
        return;
      }
      const activeEntry = projects.find((entry) => entry.id === activeProject);
      const sibling = activeEntry
        ? projects.find((entry) => projectSource(entry.id) === next && entry.path === activeEntry.path)
        : undefined;
      if (sibling && sibling.id !== activeProject) await selectProject(sibling.id);
    },
    [availableSources, activeProject, projects, newSession, selectProject],
  );

  // Local / Cloud segmented control shown in the composer (only when both are reachable).
  const sourceToggleNode =
    availableSources.length > 1 ? (
      <div className="flex items-center gap-0.5 rounded-control bg-inset p-0.5" role="tablist" aria-label="Send via">
        {(["local", "cloud"] as const).map((s) =>
          availableSources.includes(s) ? (
            <button
              key={s}
              type="button"
              role="tab"
              aria-selected={activeSource === s}
              title={s === "cloud" ? "Send via cloud" : "Send via local"}
              className={cn(
                "rounded-[6px] px-2 py-1 text-caption font-medium transition-colors",
                activeSource === s ? "bg-canvas text-ink shadow-sm" : "text-muted hover:text-ink",
              )}
              onClick={() => void switchSource(s)}
            >
              {s === "cloud" ? "Cloud" : "Local"}
            </button>
          ) : null,
        )}
      </div>
    ) : null;

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
        // If the deleted one is the foreground session, reopen the backend-selected session (possibly empty).
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

  // Move a session to another project. **Does not auto-switch to the target session**
  // (to avoid an unexpected jump) — when the moved one is the foreground session, the
  // source project's foreground session has been replaced by the backend, so reopen the
  // source project's current session to align the view.
  const moveSession = useCallback(
    async (project: string, name: string, targetProject: string) => {
      try {
        setUiError("");
        const bridge = await agentBridge();
        await bridge.moveSession(project, name, targetProject);
        setNavigationHistory((prev) =>
          removeLocations(prev, (entry) => entry.project === project && entry.session === name),
        );
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

  /** Enqueue an already-parsed message into the pending queue (used while busy). */
  const enqueueTurn = useCallback((turn: PendingTurn) => {
    const key = viewKey(turn.project, turn.session);
    setViews((prev) => {
      const view = prev[key] ?? emptyView();
      return { ...prev, [key]: { ...view, pending: [...view.pending, turn] } };
    });
  }, []);

  /** Remove an item from the pending queue (user undo). */
  const removePending = useCallback((key: string, id: string) => {
    setViews((prev) => {
      const view = prev[key];
      if (!view) return prev;
      return { ...prev, [key]: { ...view, pending: view.pending.filter((turn) => turn.id !== id) } };
    });
  }, []);

  /** Send a message to the model as a new turn (echo user + start an assistant streaming placeholder).
   *  `display` is the echo body (chip placeholders preserved), `text` is the plain text sent to the model. */
  const dispatchTurn = useCallback(
    async (
      project: string,
      session: string,
      text: string,
      display: string,
      refs: FileReference[],
      sendPaths: string[],
      images: string[],
      options?: { resend?: boolean; editMessageId?: string },
    ) => {
      const key = viewKey(project, session);
      if (views[key]?.busy ?? false) return;
      const assistantId = nextId();
      turnRef.current.set(key, assistantId);
      turnStartRef.current.set(key, Date.now());
      setViews((prev) => {
        const view = prev[key] ?? emptyView();
        // Edit + resend rewrites history: drop the edited user message and every reply
        // after it from the view before appending the new turn. The backend does the same
        // to the Agent + persisted log (`agent_resend`), keeping the three consistent.
        const base = options?.resend
          ? view.messages.slice(0, indexOfMessage(view.messages, options.editMessageId))
          : view.messages;
        return {
          ...prev,
          [key]: {
            ...view,
            messages: [
              ...base,
              { id: nextId(), role: "user", content: display, status: "complete", references: refs },
              { id: assistantId, role: "assistant", content: "", status: "streaming" },
            ],
            turnSegments: { ...view.turnSegments, [assistantId]: [] },
            busy: true,
            retryStatus: null,
            error: "",
          },
        };
      });

      try {
        const bridge = await agentBridge();
        if (options?.resend) {
          await bridge.resend(project, session, text, sendPaths, images);
        } else {
          await bridge.send(project, session, text, sendPaths, images);
        }
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        applyEvent(key, { type: "error", message, project, session });
      }
    },
    [views, applyEvent],
  );

  /** Submit a turn: send immediately when idle, otherwise enqueue into pending and send automatically once the previous turn ends. */
  const submit = useCallback(
    async (
      project: string,
      session: string,
      text: string,
      display: string,
      refs: FileReference[],
      sendPaths: string[],
      images: string[],
      options?: { resend?: boolean; editMessageId?: string },
    ) => {
      const key = viewKey(project, session);
      if (views[key]?.busy ?? false) {
        enqueueTurn({ id: nextId(), project, session, text, display, refs, sendPaths, images });
        return;
      }
      await dispatchTurn(project, session, text, display, refs, sendPaths, images, options);
    },
    [views, dispatchTurn, enqueueTurn],
  );

  // Auto-flush the pending queue: when a session goes idle (busy true->false) with a non-empty queue, pop the head and send.
  // Use `flushingRef` to prevent the re-render triggered by "removing the head" from popping the queue again before `busy` is set.
  const flushingRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    for (const [key, view] of Object.entries(views)) {
      if (view.busy || view.pending.length === 0) {
        flushingRef.current.delete(key);
        continue;
      }
      if (flushingRef.current.has(key)) continue;
      const [next, ...rest] = view.pending;
      flushingRef.current.add(key);
      setViews((prev) => {
        const current = prev[key];
        if (!current) return prev;
        return { ...prev, [key]: { ...current, pending: rest } };
      });
      void dispatchTurn(next.project, next.session, next.text, next.display, next.refs, next.sendPaths, next.images);
    }
  }, [views, dispatchTurn]);

  const send = useCallback(async () => {
    const project = activeProject;
    const session = sessionName;
    if (!project || !session) return;
    const key = viewKey(project, session);
    // Edit mode for the last user message: submit rewinds that turn and resends the edited
    // text (the old reply is discarded), instead of appending a new turn.
    const editing = editingTurn && editingTurn.key === key ? editingTurn : null;

    const refs = references;
    // `/` skill entry (same semantics as the TUI): the skill appears as a chip in the
    // input; the text after the chip is args. No args = activate only + a local system
    // notice (not sent to the model); with args = activate + use args as this turn's
    // prompt. Desktop has no local built-in slash commands; only a chip takes this path.
    const skill = refs.find((reference) => reference.kind === "skill");
    if (skill && !editing) {
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
        // No args: activate only, don't bother the model, show a local system notice (chip echo preserved).
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
                  content: `${CHIP_TOKEN} Skill activated (its instructions will be injected every turn from now on).`,
                  status: "complete",
                  references: [skill],
                },
              ],
            },
          };
        });
        return;
      }
      // With args: after activating, treat args as this turn's prompt and continue
      // through the normal send flow; display keeps the chip, but **the skill name is not
      // sent to the backend as a reference path** (sendPaths empty). Enqueue if running.
      await submit(project, session, args, `${CHIP_TOKEN}${args}`, [skill], [], []);
      return;
    }

    // The body sent to the model / checked for emptiness must strip placeholders
    // (placeholders are not user text); but **the echo content must keep the
    // placeholders** — `InlineReferences` places chips back into the body by placeholder
    // position, so stripping them renders not a single tag (a past bug).
    const plain = stripTokens(input).trim();
    // Images are a valid payload on their own (no text needed): allow an image-only turn.
    const images = refs.filter((reference) => reference.kind === "image").map((reference) => reference.dataUrl ?? "");
    if (!plain && images.length === 0) return;
    setInput("");
    setReferences([]);
    setUiError("");
    setEditingTurn(null);
    await submit(
      project,
      session,
      plain,
      input.trim(),
      refs,
      // Only file/dir refs travel as workspace paths; images go through the `images` channel.
      refs.filter((reference) => reference.kind !== "image").map((reference) => reference.path),
      images,
      editing ? { resend: true, editMessageId: editing.messageId } : undefined,
    );
  }, [activeProject, input, references, sessionName, slashSearch, submit, editingTurn]);

  /** Enter edit mode for a user message: pre-fill the composer; submit will resend. */
  const beginEdit = useCallback(
    (message: AiTranscriptMessage) => {
      if (!activeProject || !sessionName) return;
      const key = viewKey(activeProject, sessionName);
      if (views[key]?.busy) return;
      setEditingTurn({ key, messageId: String(message.id) });
      setInput(message.content);
      setReferences(message.references ?? []);
      setUiError("");
      requestAnimationFrame(() => editorRef.current?.focus());
    },
    [activeProject, sessionName, views],
  );

  /** Leave edit mode without resending (Esc / session switch). */
  const cancelEdit = useCallback(() => {
    setEditingTurn(null);
    setInput("");
    setReferences([]);
  }, []);

  // Esc cancels edit mode with no side effects (mirrors the TUI's `/rewind` Esc).
  useEffect(() => {
    if (!editingTurn) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        cancelEdit();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editingTurn, cancelEdit]);

  // Switching project / session abandons any in-progress edit.
  useEffect(() => {
    setEditingTurn(null);
  }, [activeProject, sessionName]);

  const stop = useCallback(async () => {
    const project = activeProject;
    const session = sessionName;
    if (!project || !session) return;
    // Only send the cancel signal: the real wrap-up (roll back this turn + set `busy`
    // false) is done uniformly by the backend-pushed `finished(cancelled)` event —
    // avoiding setting `busy=false` optimistically here, which would clash with the
    // backend's "agent is currently running" check if the user resends immediately.
    await (await agentBridge()).cancel(project, session).catch(() => {});
  }, [activeProject, sessionName]);

  const switchModel = useCallback(
    async (value: string) => {
      if (!activeProject) return;
      const epoch = openEpochRef.current;
      try {
        const bridge = await agentBridge();
        await bridge.setModel(activeProject, value, sessionName ?? undefined);
        if (epoch === openEpochRef.current) setModel(value);
      } catch (e) {
        if (epoch === openEpochRef.current) setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject, sessionName],
  );

  const view =
    activeProject && sessionName
      ? (views[viewKey(activeProject, sessionName)] ?? emptyView())
      : emptyView();
  const busy = view.busy;
  // Content-free runtime status for the foreground session (separate `status://event` channel).
  const status = activeProject && sessionName ? (statuses[viewKey(activeProject, sessionName)] ?? null) : null;
  // The user message currently being edited in place (matches the active session). While set,
  // the composer is rendered at the message's position instead of the bottom, and submit rewinds
  // that turn and resends (see `send` / `beginEdit`).
  const editing =
    editingTurn && activeProject && sessionName && editingTurn.key === viewKey(activeProject, sessionName)
      ? editingTurn
      : null;
  const error = uiError || view.error;
  // New chat (Codex-style empty session): a session is attached but has no messages yet,
  // and is not loading / running. In this state the input is centered with Chat/Work and
  // project switching below — i.e. the standalone "new chat" flow.
  const isNewChat =
    Boolean(activeProject && sessionName) &&
    !view.loading &&
    view.messages.length === 0 &&
    !view.busy;

  // Welcome content (empty-session main area): shared by the centered new-chat flow and "switching to a session with no messages".
  const heroNode = (
    <div className="chat-welcome flex flex-col items-center gap-4 px-4 py-6 text-center sm:py-10">
      <img
        src="/brand/shirley-character.png"
        alt="Shirley Fenette from Code Geass, long orange hair, wearing an Ashford Academy uniform"
        width={768}
        height={1152}
        className="h-[clamp(140px,32vh,280px)] w-auto max-w-full object-contain"
        draggable={false}
      />
      <div className="space-y-2">
        <h1 className="font-display text-title-sm text-ink">Shirley</h1>
        <p className="text-body-sm text-muted">
          Start chatting with Shirley. Type <kbd className="font-utility">@</kbd> to reference workspace files, or <kbd className="font-utility">/</kbd> to invoke a skill.
        </p>
      </div>
    </div>
  );

  // Composer (with slash / mention popovers and the model selector). Shared by the
  // centered new-chat flow and normal sessions; the popovers use `absolute bottom-full`
  // positioned relative to this container, so both must reuse the same container structure.
  const composerNode = (
    <div className="chat-composer-shell relative w-full shrink-0 px-3 pb-3">
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
        label="Send message"
        value={stripTokens(input)}
        placeholder="Message Shirley… (@ to reference files, / to invoke a skill)"
        busy={busy}
        onValueChange={() => {}}
        onSend={() => void send()}
        onStop={() => void stop()}
        leadingAction={
          <>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label="Add reference (@)"
              className="rounded-full text-muted hover:text-ink"
              onClick={() => editorRef.current?.openMention()}
            >
              <Plus />
            </Button>
            {/* Image upload entry: always present (paste alone is not discoverable). Opens
                the OS file dialog; picked files become `image` chips, the same payload shape
                as pasted images. */}
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label="Attach image"
              title="Attach image"
              className="rounded-full text-muted hover:text-ink"
              onClick={() => imageInputRef.current?.click()}
            >
              <ImagePlus />
            </Button>
            <input
              ref={imageInputRef}
              type="file"
              accept="image/*"
              multiple
              className="hidden"
              onChange={(event) => {
                const files = Array.from(event.target.files ?? []);
                // Reset so picking the same file twice still fires `change`.
                event.target.value = "";
                if (files.length > 0) void editorRef.current?.insertImageFiles(files);
              }}
            />
            {/* Task ledger toggle: the card is not resident; this is the only entry, and it
                carries a live progress badge so "is it moving" stays visible at a glance. */}
            {view.todo && activeProject && sessionName && (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className={cn(
                  "rounded-full text-muted hover:text-ink",
                  taskPanelKey === viewKey(activeProject, sessionName) && "bg-quiet text-ink",
                )}
                aria-expanded={taskPanelKey === viewKey(activeProject, sessionName)}
                aria-label="Show task list"
                title="Task list"
                onClick={() =>
                  setTaskPanelKey((prev) =>
                    prev === viewKey(activeProject, sessionName)
                      ? null
                      : viewKey(activeProject, sessionName),
                  )
                }
              >
                <ListChecks aria-hidden="true" />
                {view.todo.total > 0 && (
                  <span className="font-utility text-caption">
                    {view.todo.completed}/{view.todo.total}
                  </span>
                )}
              </Button>
            )}
          </>
        }
        trailingAction={
          <div className="flex items-center gap-1.5">
            {/* Local/Cloud send-target toggle: only offered in the new-chat flow (a fresh
                session is where the runtime is chosen). Once a session has messages the
                source is already fixed by its project id, so hide it to avoid mid-session
                surprises. */}
            {isNewChat ? sourceToggleNode : null}
            <ModelSelector project={activeProject ?? ""} current={model} onSelect={(value) => void switchModel(value)} />
          </div>
        }
        inputSlot={
          <MentionEditor
            ref={editorRef}
            id="shirley-composer"
            label="Send message"
            value={input}
            references={references}
            placeholder="Message Shirley… (@ to reference files, / to invoke a skill)"
            autoFocus={!mobile}
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
  );

  return (
    <>
      <ImageHoverPreview />
      <DesktopChrome
      title={(activeProject && sessionName && sessionTitles[viewKey(activeProject, sessionName)]) || (sessionName ? "New session" : "Shirley")}
      projectName={
        mode === "chat"
          ? `Chat · ${activeSource === "cloud" ? "Cloud" : "Local"}`
          : `${projects.find((entry) => entry.id === activeProject)?.name ?? ""}${activeSource === "cloud" ? " · Cloud" : ""}`
      }
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
      onNewChat={() => { if (mobile) setSidebarCollapsed(true); activeProject ? void newSession(activeProject) : void addProject(); }}
      onAddProject={() => void addProject()}
      onNewBrowserTab={browserAvailable ? () => openWebTab() : undefined}
      topContent={
        <HeaderTabs
          sessionTitle={
            (currentKey && sessionTitles[currentKey]) || (sessionName ? "New session" : "")
          }
          chatActive={activeWebTabId === null}
          onSelectChat={() => {
            if (currentKey) setSessionActiveWebTab((prev) => ({ ...prev, [currentKey]: null }));
          }}
          webTabs={webTabs}
          activeWebTabId={activeWebTabId}
          onSelectWebTab={(id) => {
            if (currentKey) setSessionActiveWebTab((prev) => ({ ...prev, [currentKey]: id }));
          }}
          onCloseWebTab={closeWebTab}
          onNewSession={() => {
            if (mobile) setSidebarCollapsed(true);
            if (activeProject) void newSession(activeProject);
            else void addProject();
          }}
          canNewSession={!!activeProject}
        />
      }
    >
      {/* Left sidebar: NewChat / pinned / projects (multi-project + session navigation). */}
      {mobile && !sidebarCollapsed && (
        <button type="button" className="mobile-sidebar-backdrop" aria-label="Close sidebar" onClick={() => setSidebarCollapsed(true)} />
      )}
      <SessionSidebar
        onClose={closeSidebar}
        collapsed={sidebarCollapsed}
        chatProjectIds={availableSources.map((source) => tagProject(source, CHAT_PROJECT_ID))}
        onCurrentTitleChange={updateCurrentTitle}
        projects={projects}
        activeProject={activeProject}
        current={sessionName}
        onAddProject={() => void addProject()}
        onEditProject={(entry) => setEditingProjectEntry(entry)}
        onRenameProject={(id, name) => void renameProject(id, name)}
        onArchiveProject={(id) => void archiveProject(id)}
        onSelect={(project, name) => { if (mobile) setSidebarCollapsed(true); void switchSession(project, name); }}
        onNew={(project) => { if (mobile) setSidebarCollapsed(true); void newSession(project); }}
        onRename={(project, name, title) => void renameSession(project, name, title)}
        onDelete={(project, name) => void deleteSession(project, name)}
        onMove={(project, name, targetProject) => void moveSession(project, name, targetProject)}
      />
      {/* Right main area: fixed-width centered (aligned with shiwen AiConversationSurface's max-w-190 = 760px).
          Neither the chat area nor the input stretches with the window; long text wraps at a comfortable reading width. */}
      <WorkspaceTabs
        webTabs={allWebTabs}
        activeWebTabId={activeWebTabId}
        onMeta={onWebTabMeta}
        onCloseWebTab={closeWebTab}
      >
      <main ref={mainRef} aria-label="Conversation" className="relative flex min-h-0 min-w-0 flex-1 flex-col">
        {/* Full-width column: the scroll viewport must span the whole right area so the
            wheel works over the side gutters too. The reading column itself stays 760px
            (`max-w-190`) and centered via the per-child wrappers below. */}
        <section className="flex w-full min-h-0 flex-1 flex-col">
        {view.loading ? (
          // History snapshot not yet arrived: render a skeleton first, giving the user immediate "loading message history" feedback.
          <div className="mx-auto w-full max-w-190 min-h-0 flex-1 overflow-hidden">
            <AiConversationSkeleton />
          </div>
        ) : isNewChat ? (
          // Standalone new-chat (Codex-style empty session) flow: welcome content +
          // centered input + Chat/Work and associated-project switching below. It
          // automatically degrades to the normal session layout once the first message is sent.
          <div className="flex min-h-0 flex-1 flex-col items-center justify-start md:justify-center gap-2 overflow-y-auto">
            <div className="mx-auto flex w-full max-w-190 flex-col items-center gap-2">
              {heroNode}
              {composerNode}
              <NewChatControls
                mode={mode}
                projects={projects}
                activeProject={activeProject}
                onSelectMode={(next) => void switchNewChatMode(next)}
                onSelectProject={(project) => void newSession(project)}
                onAddProject={() => void addProject()}
              />
            </div>
          </div>
        ) : (
        <AiConversationTranscript
          messages={view.messages}
          conversationKey={activeProject && sessionName ? viewKey(activeProject, sessionName) : "main"}
          viewportRef={transcriptViewportRef}
          extraContentKey={busy ? "busy" : ""}
          emptyContent={heroNode}
          editableMessageId={
            !busy && activeProject && sessionName
              ? ([...view.messages].reverse().find((message) => message.role === "user")?.id ?? null)
              : null
          }
          onEditMessage={beginEdit}
          editingMessageId={editing?.messageId ?? null}
          renderEditContent={() => composerNode}
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
          contentClassName="mx-auto w-full max-w-190"
        />
        )}
        {/* Bottom stack (subagents / status / task / pending / composer): stays pinned
            under the scroll viewport, centered at the same 760px reading width. */}
        <div className="mx-auto flex w-full max-w-190 shrink-0 flex-col">
        {!isNewChat && activeProject && sessionName && (
          // Subagents dispatched in this session (docs/subagent.md decision 10): shown inline
          // in the session page (not as a separate sidebar panel). Click a row to expand the
          // child transcript; a running child can be cancelled from its row.
          <SubagentSection
            key={`${activeProject}::${sessionName}`}
            project={activeProject}
            session={sessionName}
          />
        )}
        {!isNewChat && (
          <>
            {error && (
              <p className="mx-3 mb-1 text-body-sm text-danger-ink" role="alert">
                {error}
              </p>
            )}
            {/* Task ledger: model-maintained task progress. **Not resident** — shown only
                when the user opens it via the composer's task toggle (the toggle carries a
                live progress badge so the "is it moving" signal stays visible). */}
            {view.todo && activeProject && sessionName && taskPanelKey === viewKey(activeProject, sessionName) && (
              <TaskCard
                task={view.todo}
                onClose={() => setTaskPanelKey(null)}
              />
            )}
            {/* Single runtime status row: liveness / health / worked-for (status channel) +
                compressing / context usage (render channel). One row, no duplication. */}
            <StatusPill
              status={status}
              compressing={view.compressing}
              contextUsage={view.contextUsage}
            />
            {/* Pending queue: messages submitted early while running, sent automatically once the previous turn ends. */}
            <PendingQueue
              turns={view.pending}
              onRemove={(id) => activeProject && sessionName && removePending(viewKey(activeProject, sessionName), id)}
            />
            {/* While editing the last user message, the composer is rendered inline at that
                message's position (see the transcript above); hide the bottom one to avoid two
                editors sharing the same state. */}
            {!editing && composerNode}
          </>
        )}
        </div>
        </section>
        {/* Minimap: attached to the left edge of the chat area's outermost container (main), hover to preview past user turns.
            Shown only when the left gutter fits it — hidden outright when the window cannot fit it, never overlapping the chat content. */}
        {minimapFits && view.messages.some((message) => message.role === "user") && (
          <AiConversationMinimap
            messages={view.messages}
            viewportRef={transcriptViewportRef}
            className="left-3"
          />
        )}
      </main>
      </WorkspaceTabs>
      </DesktopChrome>
      {editingProjectEntry && (
        <EditProjectDialog
          project={editingProjectEntry}
          onCancel={() => setEditingProjectEntry(null)}
          onPickFolder={() => agentBridge().then((bridge) => bridge.pickFolder())}
          onSave={async (changes) => {
            await updateProject(editingProjectEntry.id, changes);
            setEditingProjectEntry(null);
          }}
          onRemove={() => void removeProjectFromDialog(editingProjectEntry.id)}
        />
      )}
    </>
  );
}

/**
 * Apply an event to the view (pure function).
 *
 * Merge rule mirrors the TUI `append_streaming_delta`: consecutive deltas of the same
 * kind merge into one segment, and a change of kind starts a new segment — so the render
 * order is naturally reasoning -> content -> tools -> content -> reasoning -> tools ...
 */
function applyWireEvent(
  view: SessionView,
  event: AgentEventWire,
  assistantId: string | null,
): SessionView {
  if (event.type !== "retrying") view = { ...view, retryStatus: null };
  switch (event.type) {
    case "retrying":
      return { ...view, retryStatus: `Connection interrupted · retrying (${event.attempt})` };
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
      // Task-ledger change: update only the structured view, not the message stream — the
      // task card is a "current progress" indicator independent of the transcript, and
      // every `todo` tool call by the model refreshes it.
      return { ...view, todo: event.todo };
    }
    case "compression_started": {
      // Give the status line an explicit signal during compression (mirrors the TUI's "Compressing context").
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
        turnSegments: Object.fromEntries(Object.entries(view.turnSegments).map(([id, segments]) => [
          id,
          segments.map((segment) => segment.kind === "tools" ? {
            ...segment,
            executions: segment.executions.map((execution) => execution.status === "running" ? {
              ...execution, status: "error" as const, error: "Task ended; execution status needs confirmation",
            } : execution),
          } : segment),
        ])),
        busy: false,
        compressing: false,
        messages: view.messages.map((message) =>
          message.id === assistantId ? { ...message, status: "error" } : message,
        ),
      };
    }
    case "notice": {
      // Neutral inline hint (e.g. images dropped, continuing). Appended as its own content
      // segment so it survives the turn without being mistaken for model output.
      if (!assistantId) return view;
      return {
        ...view,
        turnSegments: appendSegment(view.turnSegments, assistantId, {
          kind: "content",
          text: event.text,
        }),
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
      error_kind: event.error_kind,
    };
    next[segmentIndex] = { ...segment, executions };
  }
  return {
    ...segments,
    [assistantId]: next,
  };
}

/**
 * Rebuild the view from a session snapshot (used when subscribing to a session with
 * accumulated history).
 *
 * The snapshot is an `ItemWire` sequence (message / tools); all assistant / tool entries
 * between two user messages are merged into the **same turn** assistant (consistent with
 * the live stream's "one assistantId per turn"). Returns the view + the last assistant
 * turn id (as the landing spot for subsequent deltas).
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

  // Preserve order + merge adjacent same-kind: consecutive content / tools segments merge
  // into one, consistent with the live stream (appendSegment). No cross-kind merging; time order preserved.
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
      // Same projection as the live compact event; also compatible with old summary snapshots.
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
      // assistant / system / error land on the assistant turn.
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
      retryStatus: null,
      error: "",
      loading: false,
      todo: snapshot.todo,
      compressing: false,
      contextUsage: null,
      usage: null,
      pending: [],
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
