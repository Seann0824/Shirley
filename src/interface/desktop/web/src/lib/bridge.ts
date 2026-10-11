// SDK 接入桥：Tauri command/event <-> AgentEventWire。
//
// 设计原则（见 docs/desktop-interface.md、docs/multi-session.md、docs/projects.md）：
// 前端不重新实现 agent，只消费 Rust 侧 run_stream() 产出的 AgentEvent（经 to_wire 映射）。
//
// 多项目多会话：每个事件带来源 `project` + `session`。前端对**每个会话**开一份订阅
// （`openSession`）：先拿该会话的视图快照重建 transcript，再叠加后续事件；切走时
// `unsubscribe` 退订。项目是「已关联的工作区目录」——一个项目下挂多个会话。

import type { AgentEventWire, SessionSnapshot } from "@/types/wire";

/** 一个会话的订阅句柄：快照 + 退订。 */
export type SessionSubscription = {
  snapshot: SessionSnapshot;
  unsubscribe: () => void;
};

/** 一个可选模型（与 Rust 侧 `ModelEntryWire` 对齐）。 */
export type ModelEntry = {
  label: string;
  value: string;
  provider: string;
};

/** `@` 引用可选的条目（与 Rust 侧 `FileEntryWire` 对齐）。 */
export type FileEntry = {
  path: string;
  name: string;
  kind: "file" | "dir";
};

/** 一个项目 = 一个已关联的工作区目录（与 Rust 侧 `ProjectEntryWire` 对齐）。 */
export type ProjectEntry = {
  /** 项目标识（规范路径字符串）——改名 / 归档 / 恢复都按它。 */
  id: string;
  /** 展示名（可改名）。 */
  name: string;
  /** 工作区根目录的展示路径。 */
  path: string;
  /** 是否已归档（归档项目默认不显示，可从归档列表恢复）。 */
  archived: boolean;
  /** 最近打开时间（Unix 毫秒）——列表按它降序。 */
  last_opened_ms: number;
};

/** 一个会话（与 Rust 侧 `SessionEntryWire` 对齐）。 */
export type SessionEntry = {
  /** 唯一标识（文件 stem），切换 / 重命名 / 删除都按它。 */
  name: string;
  /** 展示用标题：自定义标题优先，否则回落 `name`。 */
  label: string;
  /** 首条用户消息摘要（空会话为空串）。 */
  preview: string;
  /** 最近修改时间（Unix 毫秒，0 = 未知）。 */
  modified_ms: number;
  /** 用户轮数。 */
  turns: number;
  /** 该会话当前是否在跑一轮（后台活跃）——列表据此显示 loading 转圈。 */
  running: boolean;
};

/** 恢复会话时回放的一条历史消息（与 Rust 侧 `HistoryMessageWire` 对齐）。 */
export type HistoryMessage = {
  role: "user" | "assistant";
  content: string;
};

export type AgentBridge = {
  /** 列出项目。`archived = true` 时含归档项目（供归档列表）。 */
  listProjects: (archived?: boolean) => Promise<ProjectEntry[]>;
  /**
   * 打开原生文件夹选择器新建项目（已登记的目录按路径去重 / 归档则恢复）。
   * 用户取消返回 `null`。
   */
  addProject: () => Promise<ProjectEntry | null>;
  /** 重命名项目：只改展示名，不动路径 / 会话数据。 */
  renameProject: (project: string, name: string) => Promise<void>;
  /** 归档项目：只置标志、不删磁盘数据（可从归档恢复）。 */
  archiveProject: (project: string) => Promise<void>;
  /** 从归档恢复项目。 */
  restoreProject: (project: string) => Promise<void>;

  /**
   * 向指定会话发起一轮。事件经该会话的订阅（`openSession`）推回，不由 send 直接回调。
   *
   * `references` 是 `@` 引用的工作区路径；Rust 侧会把它们拼进这一轮的 prompt。
   */
  send: (project: string, session: string, text: string, references: string[]) => Promise<void>;
  /** 请求取消指定会话的当前轮。 */
  cancel: (project: string, session: string) => Promise<void>;
  /**
   * 订阅一个会话：返回视图快照（重建 transcript）+ 退订句柄。之后该会话的事件都会
   * 经 `onEvent` 推回（含后台会话——`project` / `session` 字段标明来源）。
   */
  openSession: (
    project: string,
    session: string,
    onEvent: (event: AgentEventWire) => void,
  ) => Promise<SessionSubscription>;
  /** 指定项目当前会话的模型名，用于页脚展示。 */
  modelName: (project: string) => Promise<string>;
  /** 列出可选模型（模型目录由 Rust 侧 `ModelCatalog` 提供，全局）。 */
  listModels: () => Promise<ModelEntry[]>;
  /** 热切换模型：不重建 Agent，只换 `ModelConfig::model`。 */
  setModel: (project: string, value: string) => Promise<void>;
  /** 按关键词检索工作区文件（`@` 引用用，纯应用层）。 */
  searchFiles: (project: string, query: string) => Promise<FileEntry[]>;
  /** 列出某项目的全部会话（按最近修改降序）。 */
  listSessions: (project: string) => Promise<SessionEntry[]>;
  /** 某项目当前会话名（无则 null）。 */
  currentSession: (project: string) => Promise<string | null>;
  /** 新建会话并可命名（title 为空 = 匿名），并切换过去。 */
  newSession: (project: string, title: string | null) => Promise<SessionEntry>;
  /** 切换到指定会话。 */
  switchSession: (project: string, name: string) => Promise<void>;
  /** 重命名会话：只改标题，不改标识 / 内容。 */
  renameSession: (project: string, name: string, title: string) => Promise<void>;
  /** 删除会话（连同日志与标题）。 */
  deleteSession: (project: string, name: string) => Promise<void>;
  /** 回放某项目当前会话历史（切换 / 启动后重建 transcript 用）。 */
  loadHistory: (project: string) => Promise<HistoryMessage[]>;
};

const isTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>);

/** 路由键：跨项目不重名（会话名在项目内唯一，项目 id 全局唯一）。 */
function routeKey(project: string, session: string): string {
  return `${project}::${session}`;
}

async function createTauriBridge(): Promise<AgentBridge> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  // 每个会话一个 handler，按事件里的 `project` + `session` 路由。`listen` 只注册一次——
  // 若每轮都注册新监听器，旧的不会注销、事件会被重复消费（曾经的 bug）。
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
    async send(project, session, text, references) {
      await invoke("agent_send", { project, text, references, session });
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
    async listModels() {
      return invoke<ModelEntry[]>("agent_list_models");
    },
    async setModel(project, value) {
      await invoke("agent_set_model", { project, model: value });
    },
    async searchFiles(project, query) {
      return invoke<FileEntry[]>("agent_search_files", { project, query });
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
    async loadHistory(project) {
      return invoke<HistoryMessage[]>("agent_load_history", { project });
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
      const id = `mock:/projects/新项目-${++mockProjectSeq}`;
      const entry: ProjectEntry = {
        id,
        name: `新项目-${mockProjectSeq}`,
        path: `/projects/新项目-${mockProjectSeq}`,
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
    async send(project, session, text, references) {
      const key = routeKey(project, session);
      cancelled.delete(key);
      const emit = (event: AgentEventWire) => {
        if (!cancelled.has(key)) handlers.get(key)?.({ ...event, project, session });
      };
      void (async () => {
        if (references.length > 0) {
          emit({ type: "content_delta", text: `（mock）引用 ${references.join(", ")} · ` } as AgentEventWire);
        }
        emit({ type: "content_delta", text: "（mock）收到：" } as AgentEventWire);
        for (const ch of text) {
          await new Promise((r) => setTimeout(r, 20));
          if (cancelled.has(key)) return;
          emit({ type: "content_delta", text: ch } as AgentEventWire);
        }
        emit({ type: "finished", stop_reason: "completed" } as AgentEventWire);
      })();
    },
    async cancel(project, session) {
      cancelled.add(routeKey(project, session));
    },
    async openSession(project, session, onEvent) {
      const key = routeKey(project, session);
      handlers.set(key, onEvent);
      const snapshot: SessionSnapshot = { session, items: [], running: false };
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
      // mock：不持久化，仅让选择器有反馈。
    },
    async searchFiles(_project, query) {
      // mock：返回几个假条目，让 `@` 弹层在纯浏览器下可调试。
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
    async loadHistory() {
      return [];
    },
  };
}

// mock 状态：纯浏览器 `npm run dev` 下让多项目 / 多会话 UI 可独立调试。
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
    name: "旧项目",
    path: "/projects/old",
    archived: true,
    last_opened_ms: Date.now() - 86_400_000,
  },
];
const mockSessionsByProject = new Map<string, SessionEntry[]>([
  [
    "mock:/projects/shirley",
    [
      { name: "mock-1", label: "迁移任务", preview: "把 TS 后端迁到 Rust", modified_ms: Date.now() - 60_000, turns: 3, running: false },
      { name: "mock-2", label: "mock-2", preview: "写一个 bash 工具", modified_ms: Date.now() - 3_600_000, turns: 1, running: false },
    ],
  ],
  [
    "mock:/projects/shiwen",
    [
      { name: "mock-3", label: "拾文 UI", preview: "对齐 markdown 渲染", modified_ms: Date.now() - 120_000, turns: 2, running: false },
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
