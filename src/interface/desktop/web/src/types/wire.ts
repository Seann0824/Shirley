// AgentEvent 的线格式 DTO。Rust 侧 M2 会实现 to_wire(AgentEvent) 映射；
// 这里先固化契约，前端只依赖这份类型。

export type StopReason = "completed" | "max_steps_reached" | "cancelled";

export type UsageWire = {
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number | null;
  cache_reported_input_tokens: number | null;
};

export type AgentEventWire =
  | { type: "content_delta"; text: string }
  | { type: "reasoning_delta"; text: string }
  | { type: "message_added"; role: "user" | "assistant" | "tool" }
  | { type: "tool_started"; call_id: string; name: string; arguments: string }
  | {
      type: "tool_finished";
      call_id: string;
      name: string;
      ok: boolean;
      output: string;
      elapsed_ms: number;
    }
  | { type: "usage"; usage: UsageWire }
  | { type: "context_usage"; used_tokens: number; limit_tokens: number }
  | { type: "compression_started" }
  | { type: "compression_finished" }
  | { type: "error"; message: string }
  | { type: "finished"; stop_reason: StopReason };
