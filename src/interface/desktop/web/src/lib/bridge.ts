// SDK bridge: Tauri command/event <-> AgentEventWire.
//
// Design principles (see docs/desktop-interface.md, docs/multi-session.md, docs/projects.md):
// The frontend does not reimplement the agent; it only consumes the AgentEvent produced
// by the Rust side's run_stream() (mapped via to_wire).
//
// Multi-project multi-session: every event carries its source `project` + `session`. The
// frontend opens one subscription **per session** (`openSession`): first take that
// session's view snapshot to rebuild the transcript, then layer on subsequent events;
// `unsubscribe` on switching away. A project is an "associated workspace directory" —
// one project hosts multiple sessions.

import type { AgentEventWire, SessionSnapshot } from "@/types/wire";

/** Subscription handle for one session: snapshot + unsubscribe. */
export type SessionSubscription = {
  snapshot: SessionSnapshot;
  unsubscribe: () => void;
};

/** A selectable model (aligned with Rust's `ModelEntryWire`). */
export type ModelEntry = {
  label: string;
  value: string;
  provider: string;
};

/** Entry selectable for `@` references (aligned with Rust's `FileEntryWire`). */
export type FileEntry = {
  path: string;
  name: string;
  kind: "file" | "dir";
};

/** `/` skill candidate entry (aligned with Rust's `SkillEntryWire`). */
export type SkillEntry = {
  /** Skill name (matched by the `/<name>` entry). */
  name: string;
  /** Skill description (candidate subtitle). */
  description: string;
};

/** A project = an associated workspace directory (aligned with Rust's `ProjectEntryWire`). */
export type ProjectEntry = {
  /** Project id (canonical path string) — used for rename / archive / restore. */
  id: string;
  /** Display name (renameable). */
  name: string;
  /** Display path of the workspace root. */
  path: string;
  /** Whether archived (archived projects are hidden by default and can be restored from the archive list). */
  archived: boolean;
  /** Last opened time (Unix ms) — the list is sorted by it descending. */
  last_opened_ms: number;
};

/** A session (aligned with Rust's `SessionEntryWire`). */
export type SessionEntry = {
  /** Unique id (file stem), used for switch / rename / delete. */
  name: string;
  /** Display title: custom title first, otherwise falls back to `name`. */
  label: string;
  /** First user message excerpt (empty string for an empty session). */
  preview: string;
  /** Last modified time (Unix ms, 0 = unknown). */
  modified_ms: number;
  /** Number of user turns. */
  turns: number;
  /** Whether this session is currently running a turn (active in the background) — the list shows a loading spinner based on this. */
  running: boolean;
};

/** A history message replayed when restoring a session (aligned with Rust's `HistoryMessageWire`). */
export type HistoryMessage = {
  role: "user" | "assistant";
  content: string;
};

/** A subagent summary (aligned with Rust's `SubagentSummaryWire`). */
export type SubagentSummary = {
  /** Subagent id (file stem of `<id>.jsonl`). */
  id: string;
  /** `spawn` (fresh child) or `fork` (inherits the parent transcript). */
  kind: "spawn" | "fork";
  /** The task the child was given. */
  task: string;
  /** Lifecycle status: running / completed / failed / cancelled. */
  status: "running" | "completed" | "failed" | "cancelled";
  started_ms: number;
  /** Terminal time; `null` while running. */
  finished_ms: number | null;
  elapsed_ms: number;
  /** Conclusion (empty while running). */
  summary: string;
  /** Terminal stop reason (empty while running). */
  stop_reason: string;
};

export type AgentBridge = {
  /** List projects. `archived = true` includes archived projects (for the archive list). */
  listProjects: (archived?: boolean) => Promise<ProjectEntry[]>;
  /**
   * Open the native folder picker to create a new project (already-registered
   * directories are deduplicated by path / restored if archived). Returns `null` if the
   * user cancels.
   */
  addProject: () => Promise<ProjectEntry | null>;
  /** Rename a project: display name only; path / session data untouched. */
  renameProject: (project: string, name: string) => Promise<void>;
  /** Archive a project: only sets the flag, no disk data deleted (restorable from the archive). */
  archiveProject: (project: string) => Promise<void>;
  /** Restore a project from the archive. */
  restoreProject: (project: string) => Promise<void>;

  /**
   * Start a turn for the given session. Events are pushed back via that session's
   * subscription (`openSession`), not through a direct send callback.
   *
   * `references` are the workspace paths from `@` references; the Rust side composes
   * them into this turn's prompt. `images` are pasted images as `data:` URLs; they are
   * sent as structured `ContentPart::Image` parts (not folded into the prompt text).
   */
  send: (
    project: string,
    session: string,
    text: string,
    references: string[],
    images: string[],
  ) => Promise<void>;
  /**
   * Edit the **last user message** and resend: rewinds that turn (drops the old user
   * message and every reply after it, in memory and on disk) and sends `text` as a new
   * turn. Same "option A" semantic as the TUI's `/rewind`.
   */
  resend: (
    project: string,
    session: string,
    text: string,
    references: string[],
    images: string[],
  ) => Promise<void>;
  /** Request cancellation of the given session's current turn. */
  cancel: (project: string, session: string) => Promise<void>;
  /**
   * Subscribe to a session: returns a view snapshot (to rebuild the transcript) + an
   * unsubscribe handle. Subsequent events for that session are pushed back via
   * `onEvent` (including background sessions — the `project` / `session` fields mark
   * the source).
   */
  openSession: (
    project: string,
    session: string,
    onEvent: (event: AgentEventWire) => void,
  ) => Promise<SessionSubscription>;
  /** Model name of the given project's current session, for footer display. */
  modelName: (project: string) => Promise<string>;
  /** List the models available from the current project's endpoint. */
  listModels: (project: string) => Promise<ModelEntry[]>;
  /** Hot-swap the model: no Agent rebuild, only `ModelConfig::model`. */
  setModel: (project: string, value: string) => Promise<void>;
  /** Search workspace files by keyword (for `@` references, pure application layer). */
  searchFiles: (project: string, query: string) => Promise<FileEntry[]>;
  /** List the skills visible to a project's current session (for `/` candidates). */
  listSkills: (project: string) => Promise<SkillEntry[]>;
  /** Activate a skill in a given session of a project (the `/<skill-name>` entry, same semantics as the TUI). */
  activateSkill: (project: string, session: string, name: string) => Promise<void>;
  /** List all sessions of a project (most recently modified first). */
  listSessions: (project: string) => Promise<SessionEntry[]>;
  /** A project's current session name (null if none). */
  currentSession: (project: string) => Promise<string | null>;
  /** Create a session, optionally named (empty title = anonymous), and switch to it. */
  newSession: (project: string, title: string | null) => Promise<SessionEntry>;
  /** Switch to the given session. */
  switchSession: (project: string, name: string) => Promise<void>;
  /** Rename a session: title only, not the identity / content. */
  renameSession: (project: string, name: string, title: string) => Promise<void>;
  /** Delete a session (log and title included). */
  deleteSession: (project: string, name: string) => Promise<void>;
  /** Move a session to another project (cross-project log and title migration). Does not auto-switch to the target session. */
  moveSession: (project: string, name: string, targetProject: string) => Promise<void>;
  /** Replay a project's current session history (used to rebuild the transcript after switching / startup). */
  loadHistory: (project: string) => Promise<HistoryMessage[]>;

  /** List a session's subagents (from the in-memory registry, seeded from `index.jsonl`). */
  listSubagents: (project: string, session: string) => Promise<SubagentSummary[]>;
  /** Replay a subagent's transcript (`<id>.jsonl`) — the same `HistoryMessage` shape as `loadHistory`. */
  loadSubagent: (project: string, session: string, id: string) => Promise<HistoryMessage[]>;
  /** Cancel a running subagent (idempotent no-op once terminal / unknown). */
  cancelSubagent: (project: string, session: string, id: string) => Promise<void>;
};

const isTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>);

/** Route key: unique across projects (session names unique within a project, project ids globally). */
function routeKey(project: string, session: string): string {
  return `${project}::${session}`;
}

async function createTauriBridge(): Promise<AgentBridge> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  // One handler per session, routed by the event's `project` + `session`. `listen` is
  // registered only once — registering a new listener every turn would leave old ones
  // un-unregistered and consume events multiple times (a past bug).
  const handlers = new Map<string, (event: AgentEventWire) => void>();
  await listen<AgentEventWire>("agent://event", (event) => {
    const wire = event.payload;
    if (wire.project == null || wire.session == null) return;
    handlers.get(routeKey(wire.project, wire.session))?.(wire);
  });
  return {
    async listProjects(archived) {
      return invoke<ProjectEntry[]>("agent_list_projects", { archived: archived ?? false });
    },
    async addProject() {
      return invoke<ProjectEntry | null>("agent_add_project");
    },
    async renameProject(project, name) {
      await invoke("agent_rename_project", { project, name });
    },
    async archiveProject(project) {
      await invoke("agent_archive_project", { project });
    },
    async restoreProject(project) {
      await invoke("agent_restore_project", { project });
    },
    async send(project, session, text, references, images) {
      await invoke("agent_send", { project, text, references, images, session });
    },
    async resend(project, session, text, references, images) {
      await invoke("agent_resend", { project, text, references, images, session });
    },
    async cancel(project, session) {
      await invoke("agent_cancel", { project, session });
    },
    async openSession(project, session, onEvent) {
      const key = routeKey(project, session);
      handlers.set(key, onEvent);
      const snapshot = await invoke<SessionSnapshot>("agent_subscribe", { project, session });
      return {
        snapshot,
        unsubscribe: () => {
          handlers.delete(key);
          void invoke("agent_unsubscribe", { project, session });
        },
      };
    },
    async modelName(project) {
      return invoke<string>("agent_model_name", { project });
    },
    async listModels(project) {
      return invoke<ModelEntry[]>("agent_list_models", { project });
    },
    async setModel(project, value) {
      await invoke("agent_set_model", { project, model: value });
    },
    async searchFiles(project, query) {
      return invoke<FileEntry[]>("agent_search_files", { project, query });
    },
    async listSkills(project) {
      return invoke<SkillEntry[]>("agent_list_skills", { project });
    },
    async activateSkill(project, session, name) {
      await invoke("agent_activate_skill", { project, session, name });
    },
    async listSessions(project) {
      return invoke<SessionEntry[]>("agent_list_sessions", { project });
    },
    async currentSession(project) {
      return invoke<string | null>("agent_current_session", { project });
    },
    async newSession(project, title) {
      return invoke<SessionEntry>("agent_new_session", { project, title });
    },
    async switchSession(project, name) {
      await invoke("agent_switch_session", { project, name });
    },
    async renameSession(project, name, title) {
      await invoke("agent_rename_session", { project, name, title });
    },
    async deleteSession(project, name) {
      await invoke("agent_delete_session", { project, name });
    },
    async moveSession(project, name, targetProject) {
      await invoke("agent_move_session", { project, name, targetProject });
    },
    async loadHistory(project) {
      return invoke<HistoryMessage[]>("agent_load_history", { project });
    },
    async listSubagents(project, session) {
      return invoke<SubagentSummary[]>("agent_list_subagents", { project, session });
    },
    async loadSubagent(project, session, id) {
      return invoke<HistoryMessage[]>("agent_load_subagent", { project, session, id });
    },
    async cancelSubagent(project, session, id) {
      await invoke("agent_cancel_subagent", { project, session, id });
    },
  };
}

function createMockBridge(): AgentBridge {
  const handlers = new Map<string, (event: AgentEventWire) => void>();
  const cancelled = new Set<string>();
  return {
    async listProjects(archived) {
      return mockProjects
        .filter((p) => (archived ?? false) || !p.archived)
        .sort((a, b) => b.last_opened_ms - a.last_opened_ms)
        .map((p) => ({ ...p }));
    },
    async addProject() {
      const id = `mock:/projects/new-project-${++mockProjectSeq}`;
      const entry: ProjectEntry = {
        id,
        name: `new-project-${mockProjectSeq}`,
        path: `/projects/new-project-${mockProjectSeq}`,
        archived: false,
        last_opened_ms: Date.now(),
      };
      mockProjects.push(entry);
      mockSessionsByProject.set(id, []);
      return { ...entry };
    },
    async renameProject(project, name) {
      const entry = mockProjects.find((p) => p.id === project);
      if (entry) entry.name = name.trim() || entry.name;
    },
    async archiveProject(project) {
      const entry = mockProjects.find((p) => p.id === project);
      if (entry) entry.archived = true;
    },
    async restoreProject(project) {
      const entry = mockProjects.find((p) => p.id === project);
      if (entry) {
        entry.archived = false;
        entry.last_opened_ms = Date.now();
      }
    },
    async send(project, session, text, references, images) {
      const key = routeKey(project, session);
      cancelled.delete(key);
      const emit = (event: AgentEventWire) => {
        if (!cancelled.has(key)) handlers.get(key)?.({ ...event, project, session });
      };
      void (async () => {
        if (references.length > 0) {
          emit({ type: "content_delta", text: `(mock) references ${references.join(", ")} · ` } as AgentEventWire);
        }
        if (images.length > 0) {
          emit({ type: "content_delta", text: `(mock) ${images.length} image(s) · ` } as AgentEventWire);
        }
        emit({ type: "content_delta", text: "(mock) received: " } as AgentEventWire);
        for (const ch of text) {
          await new Promise((r) => setTimeout(r, 20));
          if (cancelled.has(key)) return;
          emit({ type: "content_delta", text: ch } as AgentEventWire);
        }
        emit({ type: "finished", stop_reason: "completed" } as AgentEventWire);
      })();
    },
    // Mock keeps no persisted history, so "edit + resend" behaves like a plain send.
    async resend(project, session, text, references, images) {
      await this.send(project, session, text, references, images);
    },
    async cancel(project, session) {
      cancelled.add(routeKey(project, session));
    },
    async openSession(project, session, onEvent) {
      const key = routeKey(project, session);
      handlers.set(key, onEvent);
      const snapshot: SessionSnapshot = { session, items: [], running: false, todo: null };
      return {
        snapshot,
        unsubscribe: () => {
          handlers.delete(key);
        },
      };
    },
    async modelName() {
      return "mock-model";
    },
    async listModels() {
      return [
        { label: "mock-model", value: "mock-model", provider: "mock" },
        { label: "mock-model-mini", value: "mock-model-mini", provider: "mock" },
      ];
    },
    async setModel() {
      // mock: no persistence, just gives the selector feedback.
    },
    async searchFiles(_project, query) {
      // mock: return a few fake entries so the `@` popover is debuggable in a plain browser.
      const all: FileEntry[] = [
        { path: "src/main.rs", name: "main.rs", kind: "file" },
        { path: "src/bootstrap.rs", name: "bootstrap.rs", kind: "file" },
        { path: "src/prompt.rs", name: "prompt.rs", kind: "file" },
        { path: "src/interface", name: "interface", kind: "dir" },
        { path: "README.md", name: "README.md", kind: "file" },
      ];
      const q = query.trim().toLowerCase();
      if (!q) return all;
      return all.filter((entry) => entry.path.toLowerCase().includes(q));
    },
    async listSkills() {
      // mock: return a few fake skills so the `/` popover is debuggable in a plain browser.
      return [
        { name: "commit", description: "Split and commit changes by logical theme" },
        { name: "review", description: "Review the current changes and give suggestions" },
        { name: "migrate", description: "Guidance for migrating a TS backend to Rust" },
      ];
    },
    async activateSkill() {
      // mock: activation has no side effect (the backend writes SkillRuntime).
    },
    async listSessions(project) {
      return mockSessions(project).map((s) => ({ ...s }));
    },
    async currentSession(project) {
      return mockCurrentByProject.get(project) ?? null;
    },
    async newSession(project, title) {
      const name = `mock-${++mockSeq}`;
      const entry: SessionEntry = {
        name,
        label: title?.trim() || name,
        preview: "",
        modified_ms: Date.now(),
        turns: 0,
        running: false,
      };
      const list = mockSessionsByProject.get(project) ?? [];
      list.push(entry);
      mockSessionsByProject.set(project, list);
      mockCurrentByProject.set(project, name);
      return { ...entry };
    },
    async switchSession(project, name) {
      mockCurrentByProject.set(project, name);
    },
    async renameSession(project, name, title) {
      const entry = mockSessions(project).find((s) => s.name === name);
      if (entry) entry.label = title.trim() || name;
    },
    async deleteSession(project, name) {
      const list = mockSessionsByProject.get(project) ?? [];
      mockSessionsByProject.set(
        project,
        list.filter((s) => s.name !== name),
      );
      if (mockCurrentByProject.get(project) === name) mockCurrentByProject.delete(project);
    },
    async moveSession(project, name, targetProject) {
      const list = mockSessionsByProject.get(project) ?? [];
      const entry = list.find((s) => s.name === name);
      if (!entry) return;
      mockSessionsByProject.set(
        project,
        list.filter((s) => s.name !== name),
      );
      const target = mockSessionsByProject.get(targetProject) ?? [];
      target.push(entry);
      mockSessionsByProject.set(targetProject, target);
      if (mockCurrentByProject.get(project) === name) mockCurrentByProject.delete(project);
    },
    async loadHistory() {
      return [];
    },
    async listSubagents() {
      return [];
    },
    async loadSubagent() {
      return [];
    },
    async cancelSubagent() {
      // mock: no-op.
    },
  };
}

// mock state: lets the multi-project / multi-session UI be debugged independently under a plain-browser `npm run dev`.
let mockProjectSeq = 1;
let mockSeq = 0;
const mockProjects: ProjectEntry[] = [
  {
    id: "mock:/projects/shirley",
    name: "Shirley",
    path: "/projects/shirley",
    archived: false,
    last_opened_ms: Date.now(),
  },
  {
    id: "mock:/projects/shiwen",
    name: "shiwen",
    path: "/projects/shiwen",
    archived: false,
    last_opened_ms: Date.now() - 3_600_000,
  },
  {
    id: "mock:/projects/old",
    name: "old project",
    path: "/projects/old",
    archived: true,
    last_opened_ms: Date.now() - 86_400_000,
  },
];
const mockSessionsByProject = new Map<string, SessionEntry[]>([
  [
    "mock:/projects/shirley",
    [
      { name: "mock-1", label: "Migration task", preview: "Migrate the TS backend to Rust", modified_ms: Date.now() - 60_000, turns: 3, running: false },
      { name: "mock-2", label: "mock-2", preview: "Write a bash tool", modified_ms: Date.now() - 3_600_000, turns: 1, running: false },
    ],
  ],
  [
    "mock:/projects/shiwen",
    [
      { name: "mock-3", label: "shiwen UI", preview: "Align markdown rendering", modified_ms: Date.now() - 120_000, turns: 2, running: false },
    ],
  ],
]);
const mockCurrentByProject = new Map<string, string>([
  ["mock:/projects/shirley", "mock-1"],
  ["mock:/projects/shiwen", "mock-3"],
]);
function mockSessions(project: string): SessionEntry[] {
  return [...(mockSessionsByProject.get(project) ?? [])].sort((a, b) => b.modified_ms - a.modified_ms);
}

let cached: Promise<AgentBridge> | null = null;

export function agentBridge(): Promise<AgentBridge> {
  cached ??= isTauri ? createTauriBridge() : Promise.resolve(createMockBridge());
  return cached;
}
