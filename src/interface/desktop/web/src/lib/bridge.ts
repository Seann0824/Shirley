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
  };
}

let cached: Promise<AgentBridge> | null = null;

export function agentBridge(): Promise<AgentBridge> {
  cached ??= isTauri ? createTauriBridge() : Promise.resolve(createMockBridge());
  return cached;
}
