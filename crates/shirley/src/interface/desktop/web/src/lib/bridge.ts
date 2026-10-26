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

import type { AgentEventWire, SessionSnapshot, StateSnapshotWire } from "@/types/wire";
import { providerBridge, mockProviderBridge, selectionKey, type ProviderBridge } from "@/lib/model-providers";

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
  /** Display path of the primary workspace root (agent working dir + session-storage key). */
  path: string;
  /**
   * Additional source folders attached to this project (primary excluded). A project may span
   * several local checkouts; file tools / `@` search resolve paths against the primary root first,
   * then these. Session storage stays keyed on the primary `path`.
   */
  extra_paths?: string[];
  /** Whether archived (archived projects are hidden by default and can be restored from the archive list). */
  archived: boolean;
  /** Last opened time (Unix ms) — the list is sorted by it descending. */
  last_opened_ms: number;
  /**
   * Which runtime this project lives in. Set only by the hybrid bridge (local + cloud
   * composed): the project id is namespaced `local::<path>` / `cloud::<path>` and this
   * is the denormalised source for display (a sidebar badge). Omitted by single-source
   * bridges (backwards compatible).
   */
  source?: "local" | "cloud";
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

/** Bridge capability description (lets the UI adapt to native vs. cloud transports). */
export type BridgeCapabilities = {
  configurationScope: "local" | "remote" | "demo";
  /** Whether a native folder picker exists (Tauri desktop = true; cloud HTTP = false, use manual path input). */
  nativeFolderPicker: boolean;
  /**
   * Data sources this bridge can reach. A single-source bridge has one entry; the hybrid
   * desktop bridge is `["local", "cloud"]` when the cloud runtime is connected. The UI
   * uses this to decide whether to offer a per-send source toggle.
   */
  sources?: Array<"local" | "cloud">;
};

export type AgentBridge = {
  providers: ProviderBridge;
  /** List projects. `archived = true` includes archived projects (for the archive list). */
  listProjects: (archived?: boolean) => Promise<ProjectEntry[]>;
  /**
   * Open the native folder picker to create a new project (already-registered
   * directories are deduplicated by path / restored if archived). Returns `null` if the
   * user cancels.
   */
  addProject: () => Promise<ProjectEntry | null>;
  /**
   * Cloud (server) mode only: create/open a project from a server-side absolute path.
   * Tauri mode does not implement this (calling it throws).
   */
  addProjectByPath: (path: string, name?: string | null) => Promise<ProjectEntry | null>;
  /** Describe this bridge's capabilities (native folder picker availability etc.). */
  capabilities: () => BridgeCapabilities;
  /** Rename a project: display name only; path / session data untouched. */
  renameProject: (project: string, name: string) => Promise<void>;
  /** Archive a project: only sets the flag, no disk data deleted (restorable from the archive). */
  archiveProject: (project: string) => Promise<void>;
  /** Restore a project from the archive. */
  restoreProject: (project: string) => Promise<void>;
  /**
   * Edit a project (the "Edit project" dialog): rename and/or replace its source-folder set.
   * `folders` is a whole replacement; `primary` (when given) must be one of them. Changing the
   * primary folder moves the agent working dir + session storage, so the runtime is rebuilt.
   * Returns the updated entry (with `extra_paths` recomputed).
   */
  updateProject: (
    project: string,
    changes: { name?: string; primary?: string; folders?: string[] },
  ) => Promise<ProjectEntry>;
  /**
   * Open the native folder picker and return the chosen absolute path **without registering it**
   * (used by the "Add folder" button inside the Edit-project dialog). Returns `null` if cancelled
   * or when no native picker is available (cloud / mock modes).
   */
  pickFolder: () => Promise<string | null>;

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
   * Subscribe to the **content-free** runtime-status stream (`status://event` on desktop,
   * `/api/status` SSE on the cloud). Separate from `openSession`: the status channel ticks
   * even when no `AgentEvent` arrives (that is what makes stall detection possible), and it
   * carries no prompt / output / tool-argument text. Returns an unsubscribe handle.
   */
  subscribeStatus: (onStatus: (snapshot: StateSnapshotWire) => void) => Promise<() => void>;
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
  setModel: (project: string, value: string, session?: string) => Promise<void>;
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

  /**
   * Resolve a content-addressed artifact (`sha256:<64 hex>`) to a URL the DOM can load
   * (an `<img>` / `<video>` / `<audio>` `src`). Desktop returns an inline `data:` URL
   * (there is no HTTP server in the webview); the cloud build returns the server's
   * `/api/artifact/{sha256}` URL with the access token as a query param (media elements
   * cannot attach an `Authorization` header). Rejects on a missing artifact.
   */
  artifactUrl: (artifact: string) => Promise<string>;
};

const isTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>);

/** Route key: unique across projects (session names unique within a project, project ids globally). */
function routeKey(project: string, session: string): string {
  return `${project}::${session}`;
}

/** Strip the `sha256:` scheme from a content-addressed artifact id, leaving bare hex. */
export function stripArtifactScheme(artifact: string): string {
  return artifact.startsWith("sha256:") ? artifact.slice("sha256:".length) : artifact;
}

/** Buffer until the snapshot arrives, then discard cloud events already included in its cursor. */
function eventRouter() {
  type Route = { onEvent: (event: AgentEventWire) => void; buffer: AgentEventWire[] | null; sequence: number };
  const routes = new Map<string, Route>();
  const deliver = (route: Route, event: AgentEventWire) => {
    const sequence = (event as AgentEventWire & { sequence?: number }).sequence;
    if (sequence !== undefined) {
      if (sequence <= route.sequence) return;
      route.sequence = sequence;
    }
    route.onEvent(event);
  };
  return {
    push(event: AgentEventWire) {
      if (event.project == null || event.session == null) return;
      const route = routes.get(routeKey(event.project, event.session));
      if (!route) return;
      if (route.buffer) route.buffer.push(event);
      else deliver(route, event);
    },
    begin(project: string, session: string, onEvent: Route["onEvent"]) {
      const key = routeKey(project, session);
      const route: Route = { onEvent, buffer: [], sequence: 0 };
      routes.set(key, route);
      return {
        finish(snapshot: SessionSnapshot) {
          if (routes.get(key) !== route) return;
          route.sequence = (snapshot as SessionSnapshot & { sequence?: number }).sequence ?? 0;
          const buffer = route.buffer ?? [];
          route.buffer = null;
          for (const event of buffer) deliver(route, event);
        },
        remove() { if (routes.get(key) !== route) return false; routes.delete(key); return true; },
      };
    },
  };
}

async function createTauriBridge(remote = false): Promise<AgentBridge> {
  const { invoke: nativeInvoke } = await import("@tauri-apps/api/core");
  const invoke = <T>(command: string, args?: Record<string, unknown>): Promise<T> => remote
    ? nativeInvoke<T>("agent_remote_call", { command, args: args ?? {} })
    : nativeInvoke<T>(command, args);
  const { listen } = await import("@tauri-apps/api/event");
  // One handler per session, routed by the event's `project` + `session`. `listen` is
  // registered only once — registering a new listener every turn would leave old ones
  // un-unregistered and consume events multiple times (a past bug).
  const router = eventRouter();
  const unlisten = await listen<AgentEventWire>(remote ? "agent://remote-event" : "agent://event", (event) => router.push(event.payload));
  // Content-free status channel: a single global listener; App routes snapshots by
  // `project::session`. Absent when proxying a remote source (status is local-only for now).
  const statusListeners: Array<(snapshot: StateSnapshotWire) => void> = [];
  const statusUnlisten = remote
    ? null
    : await listen<StateSnapshotWire>("status://event", (event) => {
        for (const listener of statusListeners) listener(event.payload);
      });
  if (remote) {
    const stopConnection = await listen<boolean>("agent://remote-connection", ({ payload: connected }) => {
      window.dispatchEvent(new CustomEvent("shirley-remote-connection", { detail: connected }));
    });
    // Start the stream before requesting any snapshot, so subsequent events can be buffered.
    try { await nativeInvoke("agent_remote_start_events"); }
    catch (error) { unlisten(); stopConnection(); throw error; }
  }
  return {
    providers: providerBridge(invoke),
    async listProjects(archived) {
      return invoke<ProjectEntry[]>("agent_list_projects", { archived: archived ?? false });
    },
    async addProject() {
      if (remote) return null;
      return invoke<ProjectEntry | null>("agent_add_project");
    },
    async addProjectByPath(path, name) {
      if (!remote) throw new Error("addProjectByPath is only available in server mode");
      return invoke<ProjectEntry | null>("agent_add_project", { path, name });
    },
    capabilities() {
      return { nativeFolderPicker: !remote, configurationScope: remote ? "remote" : "local" };
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
    async updateProject(project, changes) {
      return invoke<ProjectEntry>("agent_update_project", {
        project,
        name: changes.name ?? null,
        primary: changes.primary ?? null,
        folders: changes.folders ?? null,
      });
    },
    async pickFolder() {
      if (remote) return null;
      return invoke<string | null>("agent_pick_folder");
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
    async subscribeStatus(onStatus) {
      if (statusUnlisten) statusListeners.push(onStatus);
      return () => {
        const index = statusListeners.indexOf(onStatus);
        if (index >= 0) statusListeners.splice(index, 1);
        // The shared `listen` stays registered for the app's lifetime (like `unlisten`
        // above); only the per-callback subscription is dropped here.
      };
    },
    async openSession(project, session, onEvent) {
      const route = router.begin(project, session, onEvent);
      let snapshot: SessionSnapshot;
      try { snapshot = await invoke<SessionSnapshot>("agent_subscribe", { project, session }); }
      catch (error) { route.remove(); throw error; }
      route.finish(snapshot);
      return {
        snapshot,
        unsubscribe: () => {
          if (route.remove()) void invoke("agent_unsubscribe", { project, session }).catch(() => {});
        },
      };
    },
    async modelName(project) {
      return invoke<string>("agent_model_name", { project });
    },
    async listModels(project) {
      return invoke<ModelEntry[]>("agent_list_models", { project });
    },
    async setModel(project, value, session) {
      await invoke("agent_set_model", { project, model: value, session });
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
    async artifactUrl(artifact) {
      const sha256 = stripArtifactScheme(artifact);
      return invoke<string>("agent_artifact_data", { sha256 });
    },
  };
}

function createMockBridge(): AgentBridge {
  const handlers = new Map<string, (event: AgentEventWire) => void>();
  const cancelled = new Set<string>();
  const providers = mockProviderBridge();
  return {
    providers,
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
    async addProjectByPath() {
      throw new Error("addProjectByPath is only available in server mode");
    },
    capabilities() {
      return { nativeFolderPicker: true, configurationScope: "demo" };
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
    async updateProject(project, changes) {
      const entry = mockProjects.find((p) => p.id === project);
      if (!entry) throw new Error(`unknown project: ${project}`);
      const folders = changes.folders ?? [entry.path, ...(entry.extra_paths ?? [])];
      if (folders.length === 0) throw new Error("at least one source folder is required");
      const primary = changes.primary ?? entry.path;
      if (!folders.includes(primary)) {
        throw new Error("the primary folder must be one of the source folders");
      }
      if (changes.name !== undefined && changes.name.trim()) entry.name = changes.name.trim();
      entry.path = primary;
      entry.extra_paths = folders.filter((folder) => folder !== primary);
      entry.last_opened_ms = Date.now();
      return { ...entry };
    },
    async pickFolder() {
      // No native picker in the browser mock: synthesise a unique fake absolute path.
      return `/projects/added-folder-${++mockProjectSeq}`;
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
    async subscribeStatus() {
      // mock: no runtime status stream; return a no-op unsubscribe.
      return () => {};
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
      const registry = await providers.get();
      if (registry.providers.length) return registry.providers.flatMap((provider) => provider.models.map((model) => ({
        value: selectionKey(provider.id, model.id), label: model.name || model.id, provider: provider.name,
      })));
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
    async artifactUrl(artifact) {
      // mock: no real bytes; return a transparent 1x1 PNG so the renderer path is exercisable.
      const transparentPng =
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
      void artifact;
      return transparentPng;
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


// ---------------------------------------------------------------------------
// HTTP / WebSocket transport (cloud web version)
//
// Same `AgentBridge` contract as the Tauri bridge, but the transport is the
// browser's own: commands go to `POST /api/cmd/{name}` (JSON in / JSON out) and
// events arrive on a single `GET /ws` WebSocket (every `AgentEventWire`, tagged
// with its `project` + `session`). This is what makes the cloud web build
// "the desktop frontend, with the SDK running on the server" — the UI code is
// byte-for-byte the desktop one; only this file's transport differs.
// ---------------------------------------------------------------------------

/** POST a command to the server; throws with the server's error text on non-2xx. */
async function httpCall<T>(baseUrl: string, name: string, args?: unknown): Promise<T> {
  const token = sessionStorage.getItem("shirley.cloudToken");
  const res = await fetch(`${baseUrl}/api/cmd/${name}`, {
    method: "POST",
    headers: { "content-type": "application/json", ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: JSON.stringify(args ?? {}),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(text || `request failed: ${res.status}`);
  return (text ? JSON.parse(text) : null) as T;
}

async function createHttpBridge(baseUrl: string = ""): Promise<AgentBridge> {
  const router = eventRouter();

  // One SSE stream for the whole app; events are routed by `project::session` to the
  // per-session handler registered by `openSession`. `EventSource` reconnects on drop
  // by itself, so there is no manual reconnect loop here.
  const token = sessionStorage.getItem("shirley.cloudToken");
  const es = new EventSource(`${baseUrl}/api/events${token ? `?token=${encodeURIComponent(token)}` : ""}`);
  let previouslyOpened = false;
  es.addEventListener("open", () => {
    if (previouslyOpened) window.dispatchEvent(new CustomEvent("shirley-remote-connection", { detail: true }));
    previouslyOpened = true;
  });
  es.addEventListener("error", () => window.dispatchEvent(new CustomEvent("shirley-remote-connection", { detail: false })));
  es.addEventListener("resync", () => window.dispatchEvent(new CustomEvent("shirley-remote-connection", { detail: true })));
  es.addEventListener("agent_event", (ev) => {
    let wire: AgentEventWire;
    try {
      wire = JSON.parse((ev as MessageEvent).data) as AgentEventWire;
    } catch {
      return;
    }
    router.push(wire);
  });
  // Content-free status stream on its own SSE endpoint (`/api/status`), independent of the
  // `agent_event` stream above. Routed to local listeners; App keys them by `project::session`.
  const statusListeners: Array<(snapshot: StateSnapshotWire) => void> = [];
  const statusEs = new EventSource(`${baseUrl}/api/status${token ? `?token=${encodeURIComponent(token)}` : ""}`);
  statusEs.addEventListener("status", (ev) => {
    let snapshot: StateSnapshotWire;
    try {
      snapshot = JSON.parse((ev as MessageEvent).data) as StateSnapshotWire;
    } catch {
      return;
    }
    for (const listener of statusListeners) listener(snapshot);
  });
  // No snapshot may be requested before the event stream is actually subscribed.
  await new Promise<void>((resolve, reject) => {
    const cleanup = () => { clearTimeout(timer); es.removeEventListener("open", opened); es.removeEventListener("error", failed); };
    const opened = () => { cleanup(); resolve(); };
    const failed = () => { cleanup(); es.close(); reject(new Error("Cloud event connection failed; check the connection and retry")); };
    const timer = setTimeout(failed, 10_000);
    es.addEventListener("open", opened);
    es.addEventListener("error", failed);
  });

  return {
    providers: providerBridge((name, args) => httpCall(baseUrl, name, args)),
    async listProjects(archived) {
      return httpCall<ProjectEntry[]>(baseUrl, "agent_list_projects", { archived: archived ?? false });
    },
    async addProject() {
      // Cloud mode has no native folder picker: the UI uses `addProjectByPath` instead.
      return null;
    },
    async addProjectByPath(path, name) {
      return httpCall<ProjectEntry | null>(baseUrl, "agent_add_project", { path, name });
    },
    capabilities() {
      return { nativeFolderPicker: false, configurationScope: "remote" };
    },
    async renameProject(project, name) {
      await httpCall(baseUrl, "agent_rename_project", { project, name });
    },
    async archiveProject(project) {
      await httpCall(baseUrl, "agent_archive_project", { project });
    },
    async restoreProject(project) {
      await httpCall(baseUrl, "agent_restore_project", { project });
    },
    async updateProject(project, changes) {
      return httpCall<ProjectEntry>(baseUrl, "agent_update_project", {
        project,
        name: changes.name ?? null,
        primary: changes.primary ?? null,
        folders: changes.folders ?? null,
      });
    },
    async pickFolder() {
      // Cloud mode has no native folder picker.
      return null;
    },
    async send(project, session, text, references, images) {
      await httpCall(baseUrl, "agent_send", { project, text, references, images, session });
    },
    async resend(project, session, text, references, images) {
      await httpCall(baseUrl, "agent_resend", { project, text, references, images, session });
    },
    async cancel(project, session) {
      await httpCall(baseUrl, "agent_cancel", { project, session });
    },
    async subscribeStatus(onStatus) {
      statusListeners.push(onStatus);
      return () => {
        const index = statusListeners.indexOf(onStatus);
        if (index >= 0) statusListeners.splice(index, 1);
      };
    },
    async openSession(project, session, onEvent) {
      const route = router.begin(project, session, onEvent);
      let snapshot: SessionSnapshot;
      try { snapshot = await httpCall<SessionSnapshot>(baseUrl, "agent_subscribe", { project, session }); }
      catch (error) { route.remove(); throw error; }
      route.finish(snapshot);
      return {
        snapshot,
        unsubscribe: () => {
          if (route.remove()) void httpCall(baseUrl, "agent_unsubscribe", { project, session }).catch(() => {});
        },
      };
    },
    async modelName(project) {
      return httpCall<string>(baseUrl, "agent_model_name", { project });
    },
    async listModels(project) {
      return httpCall<ModelEntry[]>(baseUrl, "agent_list_models", { project });
    },
    async setModel(project, value, session) {
      await httpCall(baseUrl, "agent_set_model", { project, model: value, session });
    },
    async searchFiles(project, query) {
      return httpCall<FileEntry[]>(baseUrl, "agent_search_files", { project, query });
    },
    async listSkills(project) {
      return httpCall<SkillEntry[]>(baseUrl, "agent_list_skills", { project });
    },
    async activateSkill(project, session, name) {
      await httpCall(baseUrl, "agent_activate_skill", { project, session, name });
    },
    async listSessions(project) {
      return httpCall<SessionEntry[]>(baseUrl, "agent_list_sessions", { project });
    },
    async currentSession(project) {
      return httpCall<string | null>(baseUrl, "agent_current_session", { project });
    },
    async newSession(project, title) {
      return httpCall<SessionEntry>(baseUrl, "agent_new_session", { project, title });
    },
    async switchSession(project, name) {
      await httpCall(baseUrl, "agent_switch_session", { project, name });
    },
    async renameSession(project, name, title) {
      await httpCall(baseUrl, "agent_rename_session", { project, name, title });
    },
    async deleteSession(project, name) {
      await httpCall(baseUrl, "agent_delete_session", { project, name });
    },
    async moveSession(project, name, targetProject) {
      await httpCall(baseUrl, "agent_move_session", { project, name, targetProject });
    },
    async loadHistory(project) {
      return httpCall<HistoryMessage[]>(baseUrl, "agent_load_history", { project });
    },
    async listSubagents(project, session) {
      return httpCall<SubagentSummary[]>(baseUrl, "agent_list_subagents", { project, session });
    },
    async loadSubagent(project, session, id) {
      return httpCall<HistoryMessage[]>(baseUrl, "agent_load_subagent", { project, session, id });
    },
    async cancelSubagent(project, session, id) {
      await httpCall(baseUrl, "agent_cancel_subagent", { project, session, id });
    },
    async artifactUrl(artifact) {
      const sha256 = stripArtifactScheme(artifact);
      const token = sessionStorage.getItem("shirley.cloudToken");
      // Media elements (`<img>` / `<video>` / `<audio>`) cannot attach an Authorization
      // header, so the token rides as a query param — the server's artifact route accepts
      // it (same reason `/api/events` does). The digest is content-addressed and immutable,
      // so the URL is safe to cache.
      return `${baseUrl}/api/artifact/${sha256}${token ? `?token=${encodeURIComponent(token)}` : ""}`;
    },
  };
}

// ---------------------------------------------------------------------------
// Hybrid bridge (desktop): local + cloud composed into ONE view
//
// The desktop can reach two independent runtime processes: the local one (Tauri
// `invoke`) and the cloud one (proxied through `agent_remote_call`). Each has its own
// ProjectRegistry + storage root, so a project / session only exists in one of them.
// To show them **mixed in one view** (not as an exclusive switch), every project id is
// namespaced with a source prefix (`local::<path>` / `cloud::<path>`), both project
// lists are merged, and each project-scoped call is routed back to the bridge that owns
// it. Events are re-tagged with the namespaced id before reaching the App, so the App's
// `project::session` view key stays globally unique with no further change.
//
// Memory is intentionally **not** part of this: personal memory stays cloud-authoritative
// (a single copy, synced to the local host by the existing sync path). This layer only
// decides "which runtime handles this send", never "which memory store is the truth".
// ---------------------------------------------------------------------------

export type DataSource = "local" | "cloud";

/** Namespace a raw project id with its source. Idempotent for already-tagged ids. */
export function tagProject(source: DataSource, rawId: string): string {
  return rawId.startsWith(`${source}::`) ? rawId : `${source}::${rawId}`;
}

/** Split a (possibly namespaced) project id into its source + raw id. Untagged ids default to local. */
export function splitProject(id: string): { source: DataSource; rawId: string } {
  if (id.startsWith("cloud::")) return { source: "cloud", rawId: id.slice("cloud::".length) };
  if (id.startsWith("local::")) return { source: "local", rawId: id.slice("local::".length) };
  return { source: "local", rawId: id };
}

/** Source of a (possibly namespaced) project id. */
export function projectSource(id: string): DataSource {
  return splitProject(id).source;
}

async function createHybridBridge(): Promise<AgentBridge> {
  const local = await createTauriBridge(false);
  // Cloud is best-effort: when it is not configured / unreachable the app still runs with
  // local only (no source toggle offered). Opening the cloud event stream is what proves
  // the connection is usable.
  let cloud: AgentBridge | null = null;
  try {
    cloud = await createTauriBridge(true);
  } catch {
    cloud = null;
  }

  /** Resolve a namespaced project id to its delegate bridge + raw id. */
  const route = (project: string): { bridge: AgentBridge; rawId: string; source: DataSource } => {
    const { source, rawId } = splitProject(project);
    if (source === "cloud") {
      if (!cloud) throw new Error("The cloud runtime is not connected. Configure the cloud endpoint in Settings first.");
      return { bridge: cloud, rawId, source };
    }
    return { bridge: local, rawId, source };
  };

  const tagEntry = (entry: ProjectEntry, source: DataSource): ProjectEntry => ({
    ...entry,
    id: tagProject(source, entry.id),
    source,
  });

  return {
    providers: local.providers,
    async listProjects(archived) {
      const [locals, clouds] = await Promise.all([
        local.listProjects(archived).catch(() => [] as ProjectEntry[]),
        cloud ? cloud.listProjects(archived).catch(() => [] as ProjectEntry[]) : Promise.resolve([] as ProjectEntry[]),
      ]);
      return [
        ...locals.map((entry) => tagEntry(entry, "local")),
        ...clouds.map((entry) => tagEntry(entry, "cloud")),
      ].sort((a, b) => b.last_opened_ms - a.last_opened_ms);
    },
    async addProject() {
      // The native folder picker only exists locally; adding a cloud project uses addProjectByPath.
      const entry = await local.addProject();
      return entry ? tagEntry(entry, "local") : null;
    },
    async addProjectByPath(path, name) {
      // Prefer the cloud runtime (the local one uses the native picker).
      if (cloud) {
        const entry = await cloud.addProjectByPath(path, name);
        return entry ? tagEntry(entry, "cloud") : null;
      }
      const entry = await local.addProjectByPath(path, name);
      return entry ? tagEntry(entry, "local") : null;
    },
    capabilities() {
      return {
        nativeFolderPicker: true,
        configurationScope: "local",
        sources: cloud ? ["local", "cloud"] : ["local"],
      };
    },
    async renameProject(project, name) {
      const { bridge, rawId } = route(project);
      await bridge.renameProject(rawId, name);
    },
    async archiveProject(project) {
      const { bridge, rawId } = route(project);
      await bridge.archiveProject(rawId);
    },
    async restoreProject(project) {
      const { bridge, rawId } = route(project);
      await bridge.restoreProject(rawId);
    },
    async updateProject(project, changes) {
      const { bridge, rawId } = route(project);
      const entry = await bridge.updateProject(rawId, changes);
      return tagEntry(entry, route(project).source);
    },
    async pickFolder() {
      return local.pickFolder();
    },
    async send(project, session, text, references, images) {
      const { bridge, rawId } = route(project);
      await bridge.send(rawId, session, text, references, images);
    },
    async resend(project, session, text, references, images) {
      const { bridge, rawId } = route(project);
      await bridge.resend(rawId, session, text, references, images);
    },
    async cancel(project, session) {
      const { bridge, rawId } = route(project);
      await bridge.cancel(rawId, session);
    },
    async subscribeStatus(onStatus) {
      // Only the local runtime emits a status channel; re-tag its snapshots so the App
      // keys them by the namespaced project id (matching the event stream).
      return local.subscribeStatus((snapshot) =>
        onStatus(
          snapshot.project == null ? snapshot : { ...snapshot, project: tagProject("local", snapshot.project) },
        ),
      );
    },
    async openSession(project, session, onEvent) {
      const { bridge, rawId, source } = route(project);
      const tagged = tagProject(source, rawId);
      // Re-tag events to the namespaced id so the App routes them to the right view.
      return bridge.openSession(rawId, session, (event) => onEvent({ ...event, project: tagged }));
    },
    async modelName(project) {
      const { bridge, rawId } = route(project);
      return bridge.modelName(rawId);
    },
    async listModels(project) {
      const { bridge, rawId } = route(project);
      return bridge.listModels(rawId);
    },
    async setModel(project, value, session) {
      const { bridge, rawId } = route(project);
      await bridge.setModel(rawId, value, session);
    },
    async searchFiles(project, query) {
      const { bridge, rawId } = route(project);
      return bridge.searchFiles(rawId, query);
    },
    async listSkills(project) {
      const { bridge, rawId } = route(project);
      return bridge.listSkills(rawId);
    },
    async activateSkill(project, session, name) {
      const { bridge, rawId } = route(project);
      await bridge.activateSkill(rawId, session, name);
    },
    async listSessions(project) {
      const { bridge, rawId } = route(project);
      return bridge.listSessions(rawId);
    },
    async currentSession(project) {
      const { bridge, rawId } = route(project);
      return bridge.currentSession(rawId);
    },
    async newSession(project, title) {
      const { bridge, rawId } = route(project);
      return bridge.newSession(rawId, title);
    },
    async switchSession(project, name) {
      const { bridge, rawId } = route(project);
      await bridge.switchSession(rawId, name);
    },
    async renameSession(project, name, title) {
      const { bridge, rawId } = route(project);
      await bridge.renameSession(rawId, name, title);
    },
    async deleteSession(project, name) {
      const { bridge, rawId } = route(project);
      await bridge.deleteSession(rawId, name);
    },
    async moveSession(project, name, targetProject) {
      const src = route(project);
      const dst = route(targetProject);
      if (src.source !== dst.source) {
        throw new Error("Cannot move a session across local / cloud runtimes.");
      }
      await src.bridge.moveSession(src.rawId, name, dst.rawId);
    },
    async loadHistory(project) {
      const { bridge, rawId } = route(project);
      return bridge.loadHistory(rawId);
    },
    async listSubagents(project, session) {
      const { bridge, rawId } = route(project);
      return bridge.listSubagents(rawId, session);
    },
    async loadSubagent(project, session, id) {
      const { bridge, rawId } = route(project);
      return bridge.loadSubagent(rawId, session, id);
    },
    async cancelSubagent(project, session, id) {
      const { bridge, rawId } = route(project);
      await bridge.cancelSubagent(rawId, session, id);
    },
    async artifactUrl(artifact) {
      // Artifacts are global (content-addressed under `~/.shirley/artifacts`), shared by the
      // local and cloud runtimes on this host. Route to local; if it is missing there, fall
      // back to cloud (whose artifact root may be a different host).
      try {
        return await local.artifactUrl(artifact);
      } catch (error) {
        if (cloud) return cloud.artifactUrl(artifact);
        throw error;
      }
    },
  };
}

let cached: Promise<AgentBridge> | null = null;

export function agentBridge(): Promise<AgentBridge> {
  // Transport selection (four states):
  // - inside Tauri + remote source     -> native authenticated HTTP/SSE proxy to the cloud
  // - inside Tauri                     -> native invoke / listen
  // - plain browser dev                -> mock (debug the UI standalone with `npm run dev`)
  // - plain browser prod               -> HTTP/SSE (same-origin, the cloud web build served by the Rust server)
  const dev = Boolean((import.meta as unknown as { env?: { DEV?: boolean } }).env?.DEV);
  // Inside the desktop app we always compose local + cloud into one hybrid view; the
  // per-send Local/Cloud choice is made in the composer, not by an exclusive reload switch.
  cached ??= isTauri
    ? createHybridBridge()
    : dev
      ? Promise.resolve(createMockBridge())
      : createHttpBridge();
  // A failed first connection must be retryable when the server comes back.
  const pending = cached;
  void pending.catch(() => { if (cached === pending) cached = null; });
  return pending;
}
