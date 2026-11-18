// Transport to the Shirley server: `POST /api/cmd/{name}` for commands, one shared
// `GET /api/events` Server-Sent-Events stream for everything the runtime emits.

import type {
  AgentEvent,
  AsrStatus,
  Transcript,
  Bot,
  LibraryEntry,
  MemoryEntry,
  PoolTask,
  SessionSnapshot,
  TaskStatus,
  SkillMarket,
  Connector,
} from "./types";
// The token arrives once in the link (`?token=`) and is kept: the installed app starts at `/`.
// It is read through `token.ts` on every request, so the gate can replace it at runtime.
import { currentToken, reportUnauthorized } from "./token";

/** A failed command, with the status kept: 401/403 means "ask for a token", not "server is down". */
export class ApiError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

/** The token was refused: whatever had it is stale, so the interface asks again. */
function refused(status: number): boolean {
  return status === 401 || status === 403;
}

function fail(status: number, body: string, fallback = ""): never {
  if (refused(status)) reportUnauthorized();
  throw new ApiError(body || fallback || `HTTP ${status}`, status);
}

async function cmd<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  const token = currentToken();
  if (token) headers.Authorization = `Bearer ${token}`;
  const response = await fetch(`/api/cmd/${name}`, {
    method: "POST",
    headers,
    body: JSON.stringify(args),
  });
  if (!response.ok) fail(response.status, await response.text().catch(() => ""), response.statusText);
  if (response.status === 204 || response.status === 202) return undefined as T;
  return (await response.json()) as T;
}

// ---------------------------------------------------------------------------
// Event stream: one EventSource, many listeners, reconnect with backoff.
// ---------------------------------------------------------------------------

type Listener = (event: AgentEvent) => void;
const listeners = new Set<Listener>();
let source: EventSource | null = null;
let reconnectDelay = 1000;

function connect() {
  if (source) return;
  const token = currentToken();
  const url = token ? `/api/events?token=${encodeURIComponent(token)}` : "/api/events";
  const es = new EventSource(url);
  source = es;
  es.addEventListener("agent_event", (raw) => {
    try {
      const event = JSON.parse((raw as MessageEvent).data) as AgentEvent;
      listeners.forEach((listener) => listener(event));
    } catch {
      // A malformed frame is dropped; the next one is independent.
    }
  });
  // The server says this client fell behind; whatever it missed is only in a fresh snapshot.
  es.addEventListener("resync", () => {
    listeners.forEach((listener) => listener({ type: "resync" }));
  });
  let opened = false;
  es.onopen = () => {
    setOnline(true);
    reconnectDelay = 1000;
    // A reconnect means a gap in the stream.
    if (opened) listeners.forEach((listener) => listener({ type: "resync" }));
    opened = true;
  };
  es.onerror = () => {
    setOnline(false);
    es.close();
    source = null;
    window.setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, 30_000);
  };
}

/** Whether the event stream is connected: a dropped one is the usual reason a turn looks dead. */
type ConnectionListener = (online: boolean) => void;
const connectionListeners = new Set<ConnectionListener>();
let online = true;
function setOnline(next: boolean) {
  if (online === next) return;
  online = next;
  connectionListeners.forEach((listener) => listener(next));
}

export function onConnection(listener: ConnectionListener): () => void {
  connectionListeners.add(listener);
  listener(online);
  return () => {
    connectionListeners.delete(listener);
  };
}

export function onAgentEvent(listener: Listener): () => void {
  listeners.add(listener);
  connect();
  return () => {
    listeners.delete(listener);
  };
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

export const api = {
  listBots: () => cmd<Bot[]>("agent_list_bots"),
  /** Which bot should hear `text`. */
  route: (text: string) => cmd<{ bot: string }>("agent_route", { text }),
  createBot: (spec: {
    name: string;
    displayName: string;
    persona: string;
    tools?: string[];
    skills?: string[];
  }) => cmd<void>("agent_create_bot", spec),
  /** Delete a bot for real: its folder goes to the server's trash, its connectors and tasks with it. */
  deleteBot: (bot: string) => cmd<void>("agent_delete_bot", { bot }),

  connectors: () => cmd<Connector[]>("agent_connectors"),
  connect: (kind: string, bot: string, settings: Record<string, string>) =>
    cmd<{ account: string }>("agent_connect", { name: kind, bot, settings }),
  disconnect: (kind: string) => cmd<void>("agent_disconnect", { name: kind }),
  /** Describe a bot; the platform designs, saves and equips it. */
  designBot: (brief: string) => cmd<{ bot: string; display_name: string }>("agent_design_bot", { brief }),
  skillMarket: (bot: string) => cmd<SkillMarket>("agent_skill_market", { bot }),
  installSkill: (bot: string, id: string) => cmd<void>("agent_skill_install", { bot, id }),
  uninstallSkill: (bot: string, id: string) => cmd<void>("agent_skill_uninstall", { bot, id }),
  subscribe: (bot: string) => cmd<SessionSnapshot>("agent_subscribe", { bot }),
  send: (bot: string, text: string, images: string[] = []) =>
    cmd<void>("agent_send", { bot, text, images }),
  /** Take back the latest message; returns its text to edit. */
  recall: (bot: string) => cmd<{ text: string }>("agent_recall", { bot }),
  cancel: (bot: string) => cmd<void>("agent_cancel", { bot }),

  poolList: () => cmd<PoolTask[]>("agent_pool_list"),
  /** Queue a task. Without `bot` the server chooses one. */
  poolEnqueue: (brief: string, bot?: string) =>
    cmd<PoolTask[]>("agent_pool_enqueue", { brief, bot }),
  poolAccept: (id: string) => cmd<PoolTask[]>("agent_pool_accept", { id }),
  poolReject: (id: string, note: string) => cmd<PoolTask[]>("agent_pool_reject", { id, note }),
  poolNote: (id: string, note: string) => cmd<PoolTask[]>("agent_pool_note", { id, note }),
  poolRetry: (id: string) => cmd<PoolTask[]>("agent_pool_retry", { id }),
  poolCancel: (id: string) => cmd<PoolTask[]>("agent_pool_cancel", { id }),
  poolRemove: (id: string) => cmd<PoolTask[]>("agent_pool_remove", { id }),
  poolTransition: (id: string, status: TaskStatus) =>
    cmd<PoolTask[]>("agent_pool_transition", { id, status }),

  memoryList: (bot: string) => cmd<MemoryEntry[]>("agent_memory_list", { bot }),

  /** A URL the browser may load for an artifact id (minted per request). */
  /** A text document from the library (an HTML page to render with `srcdoc`). */
  assetText: (bot: string, id: string) =>
    cmd<{ text: string; mime: string; truncated: boolean }>("agent_asset_text", { bot, id }),
  assetUrl: (bot: string, id: string) => cmd<string>("agent_asset_url", { bot, id }),

  /** Whether the server can transcribe speech. Asked once per page load. */
  asrStatus: (() => {
    let pending: Promise<AsrStatus> | null = null;
    return () => {
      pending ??= cmd<AsrStatus>("agent_asr_status").catch(() => ({
        available: false,
        model_ready: false,
        hint: "",
      }));
      return pending;
    };
  })(),

  /** Transcribe a WAV recording on the server. `partial` = live preview, skipped when busy. */
  async transcribe(wav: Blob, options: { partial?: boolean; language?: string | null; signal?: AbortSignal } = {}): Promise<Transcript> {
    const query = new URLSearchParams();
    if (options.partial) query.set("partial", "1");
    if (options.language) query.set("language", options.language);
    const headers: Record<string, string> = { "Content-Type": "audio/wav" };
    const token = currentToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    const response = await fetch(`/api/transcribe?${query}`, { method: "POST", headers, body: wav, signal: options.signal });
    if (!response.ok) fail(response.status, await response.text().catch(() => ""), "语音转写失败");
    return (await response.json()) as Transcript;
  },

  /** Put a file into the media library; the entry's id is what a message references. */
  async upload(bot: string, file: File): Promise<LibraryEntry> {
    const query = new URLSearchParams({
      bot,
      name: file.name,
      mime: file.type || "application/octet-stream",
    });
    const headers: Record<string, string> = {};
    const token = currentToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    const response = await fetch(`/api/upload?${query}`, { method: "POST", headers, body: file });
    if (!response.ok) fail(response.status, await response.text().catch(() => ""), response.statusText);
    return (await response.json()) as LibraryEntry;
  },
};
