// The linear segment stream that one assistant reply is cut into in event order (mirroring
// the TUI items array): content blocks / tool-group blocks, whichever happens first comes
// first. desktop previously compressed a whole turn into "body + one tool disclosure"
// and could not express interleaving like "body -> tools -> body -> tools".
//
// Reasoning does **not** enter the segment stream: the user does not care how the AI
// thinks, only what it does. The thinking process is consumed internally by the program,
// and the UI keeps only an active indicator that "the AI is still working".
import type { ToolErrorKindWire } from "./wire";

export type AiSegment =
  | { kind: "content"; text: string }
  | { kind: "compact"; text: string }
  | { kind: "tools"; executions: AiToolExecution[] };

// Migrated from shiwen's AiToolExecution, keeping only the fields Shirley's tool cards
// actually consume. shiwen-specific fields (entity / space / host_repository, etc.) are stripped.

export type AiToolExecution = {
  id: number | string;
  execution_id?: number;
  call_id?: string | null;
  content_offset?: number | null;
  tool_name: string;
  status: "pending" | "running" | "complete" | "error";
  summary: string;
  result_json?: string;
  error?: string | null;
  /// Stable error class from the SDK when this execution failed (see `ToolErrorKindWire`).
  /// Classification reads this instead of matching the error text.
  error_kind?: ToolErrorKindWire | null;
  created_at?: string;
  completed_at?: string | null;
  input?: Record<string, unknown>;
};
