// Transport to the Shirley server: `POST /api/cmd/{name}` for commands, one shared
// `GET /api/events` Server-Sent-Events stream for everything the runtime emits.

import type {
  AgentEvent,
  Bot,
  LibraryEntry,
  MemoryEntry,
  PoolTask,
  SessionSnapshot,
  TaskStatus,
} from "./types";

const token = new URLSearchParams(window.location.search).get("token") ?? undefined;

async function cmd<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;
  const response = await fetch(`/api/cmd/${name}`, {
    method: "POST",
    headers,
    body: JSON.stringify(args),
  });
  if (!response.ok) {
    throw new Error((await response.text().catch(() => "")) || response.statusText);
  }
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
  es.onopen = () => {
    reconnectDelay = 1000;
  };
  es.onerror = () => {
    es.close();
    source = null;
    window.setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, 30_000);
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
  createBot: (spec: {
    name: string;
    displayName: string;
    persona: string;
    tools?: string[];
    skills?: string[];
  }) => cmd<void>("agent_create_bot", spec),

  subscribe: (bot: string) => cmd<SessionSnapshot>("agent_subscribe", { bot }),
  send: (bot: string, text: string, images: string[] = []) =>
    cmd<void>("agent_send", { bot, text, images }),
  cancel: (bot: string) => cmd<void>("agent_cancel", { bot }),

  poolList: () => cmd<PoolTask[]>("agent_pool_list"),
  /** Queue a task. Without `bot` the server chooses one. */
  poolEnqueue: (brief: string, bot?: string) =>
    cmd<PoolTask[]>("agent_pool_enqueue", { brief, bot }),
  poolAccept: (id: string) => cmd<PoolTask[]>("agent_pool_accept", { id }),
  poolReject: (id: string, note: string) => cmd<PoolTask[]>("agent_pool_reject", { id, note }),
  poolRetry: (id: string) => cmd<PoolTask[]>("agent_pool_retry", { id }),
  poolCancel: (id: string) => cmd<PoolTask[]>("agent_pool_cancel", { id }),
  poolRemove: (id: string) => cmd<PoolTask[]>("agent_pool_remove", { id }),
  poolTransition: (id: string, status: TaskStatus) =>
    cmd<PoolTask[]>("agent_pool_transition", { id, status }),

  memoryList: (bot: string) => cmd<MemoryEntry[]>("agent_memory_list", { bot }),

  /** A URL the browser may load for an artifact id (minted per request). */
  assetUrl: (bot: string, id: string) => cmd<string>("agent_asset_url", { bot, id }),

  /** Put a file into the media library; the entry's id is what a message references. */
  async upload(bot: string, file: File): Promise<LibraryEntry> {
    const query = new URLSearchParams({
      bot,
      name: file.name,
      mime: file.type || "application/octet-stream",
    });
    const headers: Record<string, string> = {};
    if (token) headers.Authorization = `Bearer ${token}`;
    const response = await fetch(`/api/upload?${query}`, { method: "POST", headers, body: file });
    if (!response.ok) throw new Error(await response.text().catch(() => response.statusText));
    return (await response.json()) as LibraryEntry;
  },
};
