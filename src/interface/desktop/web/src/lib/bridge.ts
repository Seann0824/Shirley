// SDK 接入桥：Tauri command/event <-> AgentEventWire。
//
// 设计原则（见 docs/desktop-interface.md）：前端不重新实现 agent，
// 只消费 Rust 侧 run_stream() 产出的 AgentEvent（经 to_wire 映射）。
//
// M2 之前，Rust 侧 command 尚未落地，这里提供可切换的 mock 实现，
// 让 UI 能在纯浏览器 `npm run dev` 下独立跑起来（不依赖 Tauri 壳）。

import type { AgentEventWire } from "@/types/wire";

export type StreamHandle = { cancel: () => void };

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
};

/** 恢复会话时回放的一条历史消息（与 Rust 侧 `HistoryMessageWire` 对齐）。 */
export type HistoryMessage = {
  role: "user" | "assistant";
  content: string;
};

export type AgentBridge = {
  /**
   * 发送一条用户消息，返回事件流订阅句柄。
   *
   * `references` 是 `@` 引用的工作区路径；Rust 侧会把它们拼进这一轮的 prompt，
   * 与消息正文一起交给 Agent（引用不是 Agent 的对外契约，只是这一轮输入的一部分）。
   */
  send: (
    text: string,
    references: string[],
    onEvent: (event: AgentEventWire) => void,
  ) => Promise<StreamHandle>;
  /** 当前会话的模型名，用于页脚展示。 */
  modelName: () => Promise<string>;
  /** 列出可选模型（模型目录由 Rust 侧 `ModelCatalog` 提供）。 */
  listModels: () => Promise<ModelEntry[]>;
  /** 热切换模型：不重建 Agent，只换 `ModelConfig::model`。 */
  setModel: (value: string) => Promise<void>;
  /** 按关键词检索工作区文件（`@` 引用用，纯应用层）。 */
  searchFiles: (query: string) => Promise<FileEntry[]>;
  /** 列出全部会话（按最近修改降序）。与 TUI 的 `/session` 读同一份目录。 */
  listSessions: () => Promise<SessionEntry[]>;
  /** 当前会话名（无则 null）。 */
  currentSession: () => Promise<string | null>;
  /** 新建会话并可命名（title 为空 = 匿名），并切换过去。 */
  newSession: (title: string | null) => Promise<SessionEntry>;
  /** 切换到指定会话。 */
  switchSession: (name: string) => Promise<void>;
  /** 重命名会话：只改标题，不改标识 / 内容。 */
  renameSession: (name: string, title: string) => Promise<void>;
  /** 删除会话（连同日志与标题）。 */
  deleteSession: (name: string) => Promise<void>;
  /** 回放当前会话历史（切换 / 启动后重建 transcript 用）。 */
  loadHistory: () => Promise<HistoryMessage[]>;
};

const isTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>);

async function createTauriBridge(): Promise<AgentBridge> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  // 事件监听器**只注册一次**：Rust 侧同一时刻只跑一轮（`agent_send` 有
  // busy 门），所以只需要一个可切换的 handler。若每轮都 `listen` 一个新
  // 监听器，旧的不会注销，新事件的 delta 会被旧闭包重复消费、追加进旧消息。
  let handler: ((event: AgentEventWire) => void) | null = null;
  await listen<AgentEventWire>("agent://event", (event) => {
    handler?.(event.payload);
  });
  return {
    async send(text, references, onEvent) {
      handler = onEvent;
      await invoke("agent_send", { text, references });
      return {
        cancel: () => {
          handler = null;
          void invoke("agent_cancel");
        },
      };
    },
    async modelName() {
      return invoke<string>("agent_model_name");
    },
    async listModels() {
      return invoke<ModelEntry[]>("agent_list_models");
    },
    async setModel(value) {
      await invoke("agent_set_model", { model: value });
    },
    async searchFiles(query) {
      return invoke<FileEntry[]>("agent_search_files", { query });
    },
    async listSessions() {
      return invoke<SessionEntry[]>("agent_list_sessions");
    },
    async currentSession() {
      return invoke<string | null>("agent_current_session");
    },
    async newSession(title) {
      return invoke<SessionEntry>("agent_new_session", { title });
    },
    async switchSession(name) {
      await invoke("agent_switch_session", { name });
    },
    async renameSession(name, title) {
      await invoke("agent_rename_session", { name, title });
    },
    async deleteSession(name) {
      await invoke("agent_delete_session", { name });
    },
    async loadHistory() {
      return invoke<HistoryMessage[]>("agent_load_history");
    },
  };
}

function createMockBridge(): AgentBridge {
  return {
    async send(text, references, onEvent) {
      let cancelled = false;
      const emit = (event: AgentEventWire) => {
        if (!cancelled) onEvent(event);
      };
      void (async () => {
        if (references.length > 0) {
          emit({ type: "content_delta", text: `（mock）引用 ${references.join(", ")} · ` });
        }
        emit({ type: "content_delta", text: "（mock）收到：" });
        for (const ch of text) {
          await new Promise((r) => setTimeout(r, 20));
          if (cancelled) return;
          emit({ type: "content_delta", text: ch });
        }
        emit({ type: "finished", stop_reason: "completed" });
      })();
      return { cancel: () => (cancelled = true) };
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
    async searchFiles(query) {
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
    async listSessions() {
      return mockSessions().map((s) => ({ ...s }));
    },
    async currentSession() {
      return mockCurrentName;
    },
    async newSession(title) {
      const name = `mock-${++mockSeq}`;
      const entry: SessionEntry = {
        name,
        label: title?.trim() || name,
        preview: "",
        modified_ms: Date.now(),
        turns: 0,
      };
      mockSessionList.push(entry);
      mockCurrentName = name;
      return { ...entry };
    },
    async switchSession(name) {
      mockCurrentName = name;
    },
    async renameSession(name, title) {
      const entry = mockSessionList.find((s) => s.name === name);
      if (entry) entry.label = title.trim() || name;
    },
    async deleteSession(name) {
      mockSessionList = mockSessionList.filter((s) => s.name !== name);
      if (mockCurrentName === name) mockCurrentName = null;
    },
    async loadHistory() {
      return [];
    },
  };
}

// mock 会话状态：纯浏览器 `npm run dev` 下让会话 UI 可独立调试。
let mockSeq = 0;
let mockCurrentName: string | null = "mock-1";
let mockSessionList: SessionEntry[] = [
  {
    name: "mock-1",
    label: "迁移任务",
    preview: "把 TS 后端迁到 Rust",
    modified_ms: Date.now() - 60_000,
    turns: 3,
  },
  {
    name: "mock-2",
    label: "mock-2",
    preview: "写一个 bash 工具",
    modified_ms: Date.now() - 3_600_000,
    turns: 1,
  },
];
function mockSessions(): SessionEntry[] {
  return [...mockSessionList].sort((a, b) => b.modified_ms - a.modified_ms);
}

let cached: Promise<AgentBridge> | null = null;

export function agentBridge(): Promise<AgentBridge> {
  cached ??= isTauri ? createTauriBridge() : Promise.resolve(createMockBridge());
  return cached;
}
