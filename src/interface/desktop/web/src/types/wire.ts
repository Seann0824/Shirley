// AgentEvent / 会话快照的线格式 DTO。Rust 侧 `desktop/wire.rs` 负责映射；
// 这里固化契约，前端只依赖这份类型。多会话下每个事件都带来源会话名 `session`
// （`null` = 未打标），前端据此把事件路由到对应会话的视图。

export type StopReason = "completed" | "max_steps_reached" | "cancelled";

export type UsageWire = {
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number | null;
  cache_reported_input_tokens: number | null;
};

/** 事件来源会话名；`null` = 未打标（旧 Rust 侧 / 测试）。 */
type SessionTag = { session: string | null };

export type AgentEventWire =
  | ({ type: "content_delta"; text: string } & SessionTag)
  | ({ type: "reasoning_delta"; text: string } & SessionTag)
  | ({ type: "message_added"; role: "user" | "assistant" | "tool" } & SessionTag)
  | ({ type: "tool_started"; call_id: string; name: string; arguments: string } & SessionTag)
  | ({
      type: "tool_finished";
      call_id: string;
      name: string;
      ok: boolean;
      output: string;
      elapsed_ms: number;
    } & SessionTag)
  | ({ type: "usage"; usage: UsageWire } & SessionTag)
  | ({ type: "context_usage"; used_tokens: number; limit_tokens: number } & SessionTag)
  | ({ type: "compression_started" } & SessionTag)
  | ({ type: "compression_finished" } & SessionTag)
  | ({ type: "error"; message: string } & SessionTag)
  | ({ type: "finished"; stop_reason: StopReason } & SessionTag);

/** 快照里一条 UI 条目（与 Rust 侧 `desktop/wire.rs` 的 `ItemWire` 对齐）。 */
export type ItemWire =
  | { kind: "message"; role: string; content: string; thinking: boolean }
  | { kind: "tools"; calls: { name: string; arguments: string }[] };

/** 订阅某会话时下发的完整视图快照（与 Rust 侧 `SessionSnapshotWire` 对齐）。 */
export type SessionSnapshot = {
  session: string;
  items: ItemWire[];
  /** 该会话当前是否在跑一轮（决定前端 busy 门）。 */
  running: boolean;
};
