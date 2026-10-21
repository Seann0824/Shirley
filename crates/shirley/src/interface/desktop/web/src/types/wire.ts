// Wire-format DTOs for AgentEvent / session snapshots. The Rust side (`desktop/wire.rs`)
// does the mapping; the contract is pinned here and the frontend depends only on these
// types. With multiple projects and sessions, every event carries its source `project` +
// `session` (`null` = untagged), and the frontend routes events to the matching session view.

export type StopReason = "completed" | "max_steps_reached" | "cancelled";

/** Status of one task-ledger step (aligned with Rust `TodoStatus` serialization). */
export type TodoStatus = "pending" | "in_progress" | "completed";

/** One task-ledger step (aligned with the Rust `TaskStepWire`). */
export type TaskStepWire = { text: string; status: TodoStatus };

/** Structured task-ledger view (aligned with the Rust `TaskViewWire`). */
export type TaskViewWire = {
  goal: string | null;
  steps: TaskStepWire[];
  completed: number;
  total: number;
  current: string | null;
};

/// Stable error class emitted by the SDK on tool failure (mirrors the SDK's `ErrorKind`).
/// The frontend classifies failures by switching on this, not by matching error text.
export type ToolErrorKindWire =
  | "transport"
  | "rate_limited"
  | "server_error"
  | "bad_request"
  | "unsupported"
  | "tool_failure"
  | "internal";

export type UsageWire = {
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number | null;
  cache_reported_input_tokens: number | null;
};

/** Event source project id + session name; `null` = untagged (older Rust side / tests). */
type SessionTag = { project: string | null; session: string | null };

export type AgentEventWire =
  | ({ type: "retrying"; attempt: number; reason: string } & SessionTag)
  | ({ type: "content_delta"; text: string } & SessionTag)
  | ({ type: "message_added"; role: "user" | "assistant" | "tool" } & SessionTag)
  | ({ type: "compact"; content: string } & SessionTag)
  | ({ type: "tool_started"; call_id: string; name: string; arguments: string } & SessionTag)
  | ({
      type: "tool_finished";
      call_id: string;
      name: string;
      ok: boolean;
      output: string;
      error_kind: ToolErrorKindWire | null;
      elapsed_ms: number;
    } & SessionTag)
  | ({ type: "usage"; usage: UsageWire } & SessionTag)
  | ({ type: "context_usage"; used_tokens: number; limit_tokens: number } & SessionTag)
  | ({ type: "compression_started" } & SessionTag)
  | ({ type: "compression_finished" } & SessionTag)
  | ({ type: "todo_updated"; todo: TaskViewWire | null } & SessionTag)
  | ({ type: "error"; message: string } & SessionTag)
  | ({ type: "notice"; text: string } & SessionTag)
  /** A sensitive tool call is paused awaiting the human's decision (C 档体感). Out-of-band:
   *  it does not come from the AgentEvent stream. Answer via the `agent_approve` command with
   *  `call_id` + `allow`. The matching tool card shows the pending state until resolved. */
  | ({
      type: "approval_requested";
      call_id: string;
      tool_name: string;
      arguments: string;
    } & SessionTag)
  | ({ type: "finished"; stop_reason: StopReason } & SessionTag);

/** One UI item in a snapshot (aligned with `ItemWire` in the Rust `desktop/wire.rs`). */
export type ItemWire =
  | { kind: "message"; role: string; content: string }
  | { kind: "compact"; content: string }
  | { kind: "tools"; calls: { id: string; name: string; arguments: string }[] };

/** Full view snapshot delivered when subscribing to a session (aligned with the Rust `SessionSnapshotWire`). */
export type SessionSnapshot = {
  session: string;
  items: ItemWire[];
  /** Whether this session is currently running a turn (drives the frontend busy gate). */
  running: boolean;
  /** Current task-ledger snapshot for this session (`null` = empty / no ledger). */
  todo: TaskViewWire | null;
};

/**
 * Content-free runtime-status snapshot (aligned with the Rust `StateSnapshotWire`).
 *
 * Delivered on the **separate** `status://event` channel (cloud: `/api/status` SSE), never
 * inside `AgentEventWire`: the render channel carries content and only fires on an
 * `AgentEvent`, whereas this ticks even when nothing else happens (that is what makes stall
 * detection possible). Contains **no** prompt / output / tool-argument text.
 */
export type StateSnapshotWire = {
  project: string | null;
  session: string | null;
  /** `idle` / `running` / `stalled` / `unreachable`. */
  liveness: "idle" | "running" | "stalled" | "unreachable";
  /** `healthy` / `slow_tool` / `retrying` / `compacting` / `failed`. */
  health: "healthy" | "slow_tool" | "retrying" | "compacting" | "failed";
  /** Human-facing detail for the health state (e.g. slow tool name). Never content. */
  health_detail: string | null;
  /** `model_call` / `tool_call` / `compacting` / `waiting_input` (`null` when idle). */
  activity: "model_call" | "tool_call" | "compacting" | "waiting_input" | null;
  /** +1 per completed model call / tool call within the turn. */
  progress_seq: number;
  /** Seconds since `progress_seq` last advanced ("last real progress"). */
  seconds_since_progress: number | null;
  /** Seconds since the current turn started ("Worked for Ns"). */
  seconds_since_turn_start: number | null;
  /** True when the source that produced this snapshot is gone (render muted). */
  stale: boolean;
};
