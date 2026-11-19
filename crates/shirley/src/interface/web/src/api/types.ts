// Wire contract with the Shirley server. Mirrors `interface/wire.rs`, `interface/runtime/mod.rs`
// and `pool/mod.rs`; keep both sides in sync.

export interface Bot {
  name: string;
  display_name: string;
  tools: string[];
  skills: string[];
  persona: string | null;
}

export type TaskStatus =
  | "backlog"
  | "to_do"
  | "in_progress"
  | "in_review"
  | "blocked"
  | "done"
  | "cancelled";

export interface DeliveryFile {
  id: string;
  name: string;
  mime: string;
  bytes: number;
}

export interface Delivery {
  summary: string;
  files: DeliveryFile[];
  error?: string;
}

export interface PoolTask {
  id: string;
  bot: string;
  title: string;
  brief: string;
  status: TaskStatus;
  attempt: number;
  createdMs: number;
  startedMs?: number | null;
  finishedMs?: number | null;
  session?: string | null;
  delivery?: Delivery | null;
  note?: string | null;
  pendingNotes?: string[];
}

/** Whether the server transcribes speech (`infra/asr.rs`). */
export interface AsrStatus {
  available: boolean;
  model_ready: boolean;
  hint: string;
}

/** One transcription; `text` is null when a live preview was skipped because the server was busy. */
export interface Transcript {
  text: string | null;
  language?: string | null;
}

/** A media-library entry (`artifact::LibraryEntry`). */
export interface LibraryEntry {
  id: string;
  name: string;
  mime: string;
  bytes: number;
  addedMs: number;
}

export interface MemoryEntry {
  id: string;
  kind: string;
  scope: "shared" | "bot";
  subject: string;
  created_at: string;
  status: string;
  body: string;
}

export type AssetSource = { kind: "artifact"; id: string } | { kind: "url"; url: string };

export type RenderPart =
  | { kind: "text"; text: string }
  | { kind: "image"; source: AssetSource; alt: string; mime?: string | null }
  | {
      kind: "file";
      source?: AssetSource | null;
      path: string;
      name: string;
      mime?: string | null;
      bytes?: number | null;
    }
  | { kind: "link"; url: string; title: string }
  | { kind: "data"; schema: string; value: unknown };

export interface ToolCallWire {
  id: string;
  name: string;
  arguments: string;
  output: string | null;
  ok: boolean | null;
  parts: RenderPart[];
}

export type ItemWire =
  | { kind: "message"; role: string; content: string; parts: RenderPart[] }
  | { kind: "compact"; content: string }
  | { kind: "tools"; calls: ToolCallWire[] };

export interface SessionSnapshot {
  session: string;
  /** The newest items, oldest first. */
  items: ItemWire[];
  /** Index of `items[0]` in the whole conversation; above 0 means older items can be fetched. */
  start: number;
  running: boolean;
  sequence: number;
}

/** An older page of a conversation (`agent_history`), oldest first. */
export interface HistoryPage {
  items: ItemWire[];
  /** Index of `items[0]`; 0 means the conversation's beginning has been reached. */
  start: number;
}

interface Routed {
  bot?: string | null;
  session?: string | null;
  sequence?: number;
}

export type AgentEvent = Routed &
  (
    | { type: "retrying"; attempt: number; reason: string }
    | { type: "content_delta"; text: string }
    | { type: "message_added"; role: string }
    | { type: "compact"; content: string }
    | { type: "tool_started"; call_id: string; name: string; arguments: string }
    | {
        type: "tool_finished";
        call_id: string;
        name: string;
        ok: boolean;
        output: string;
        parts: RenderPart[];
        error_kind?: string | null;
        elapsed_ms: number;
      }
    | { type: "usage"; usage: unknown }
    | { type: "context_usage"; used_tokens: number; limit_tokens: number }
    | { type: "compaction_started" }
    | { type: "compaction_finished" }
    | { type: "todo_updated"; todo: unknown }
    | { type: "notice"; text: string }
    | { type: "finished"; stop_reason: string }
    | { type: "error"; message: string }
    /** Client-side: events may have been missed (lagged or reconnected stream); reload. */
    | { type: "resync" }
  );

/** A skill the market offers. */
export interface MarketSkill {
  id: string;
  title: string;
  summary: string;
  category: string;
  repo: string;
  ref: string;
  path: string;
  license: string;
}

/** A skill in one bot's own directory; `market` is the entry it came from, if any. */
export interface InstalledSkill {
  id: string;
  description: string;
  market: string | null;
}

export interface SkillMarket {
  market: MarketSkill[];
  installed: InstalledSkill[];
}

/** One input of a connector's connect form. */
export interface ConnectorField {
  key: string;
  label: string;
  help: string;
  secret: boolean;
}

export interface ConnectorStatus {
  /** `connecting`, `connected`, `error` or `idle`. */
  state: string;
  detail: string | null;
  events: number;
  last_event_ms: number | null;
}

/** A service bots can take work from (and answer in). */
export interface Connector {
  kind: string;
  label: string;
  fields: ConnectorField[];
  replies: boolean;
  connected: boolean;
  bot: string | null;
  values: Record<string, string> | null;
  webhook_path: string | null;
  status: ConnectorStatus;
}
