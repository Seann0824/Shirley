/**
 * bridge.ts — HTTP + SSE transport layer connecting bot-ui to Shirley's server.
 *
 * API surface:
 *   POST /api/cmd/{name}  — command dispatch (JSON body → JSON or 202 response)
 *   GET  /api/events      — SSE stream of AgentEventWire objects
 *
 * Usage:
 *   const bridge = new Bridge({ baseUrl: "http://localhost:8787", token: "..." });
 *   bridge.onEvent = (event) => { ... };
 *   bridge.connect();
 *   await bridge.send("nono", sessionId, "Hello");
 */

export interface BotWire {
  name: string;
  display_name: string;
  tools: string[];
}

export type AgentEvent =
  | { type: "content_delta"; project?: string; session?: string; text: string; sequence?: number }
  | { type: "message_added"; project?: string; session?: string; role: string; sequence?: number }
  | { type: "tool_started"; project?: string; session?: string; call_id: string; name: string; arguments: string; sequence?: number }
  | { type: "tool_finished"; project?: string; session?: string; call_id: string; name: string; ok: boolean; output: string; elapsed_ms: number; error_kind?: string; sequence?: number }
  | { type: "finished"; project?: string; session?: string; stop_reason: string; sequence?: number }
  | { type: "error"; project?: string; session?: string; message: string; sequence?: number }
  | { type: "retrying"; project?: string; session?: string; attempt: number; reason: string; sequence?: number }
  | { type: "usage"; project?: string; session?: string; usage: { input_tokens: number; output_tokens: number }; sequence?: number }
  | { type: "todo_updated"; project?: string; session?: string; todo: unknown; sequence?: number }
  | { type: "notice"; project?: string; session?: string; text: string; sequence?: number }
  | { type: "compact"; project?: string; session?: string; content: string; sequence?: number }
  | { type: "compaction_started"; project?: string; session?: string; sequence?: number }
  | { type: "compaction_finished"; project?: string; session?: string; sequence?: number }
  | { type: "context_usage"; project?: string; session?: string; used_tokens: number; limit_tokens: number; sequence?: number };

export interface BridgeOptions {
  baseUrl?: string;
  token?: string;
}

export class Bridge {
  private baseUrl: string;
  private token: string | undefined;
  private es: EventSource | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelay = 2000;

  onEvent: ((event: AgentEvent) => void) | null = null;
  onConnected: (() => void) | null = null;
  onDisconnected: (() => void) | null = null;

  constructor(options: BridgeOptions = {}) {
    this.baseUrl = options.baseUrl ?? "";
    this.token = options.token;
  }

  connect(): void {
    if (this.es) return;
    const url = this.token
      ? `${this.baseUrl}/api/events?token=${encodeURIComponent(this.token)}`
      : `${this.baseUrl}/api/events`;
    const es = new EventSource(url);
    this.es = es;

    es.addEventListener("agent_event", (e: MessageEvent) => {
      try {
        const event = JSON.parse(e.data) as AgentEvent;
        this.onEvent?.(event);
      } catch {
        // malformed — skip
      }
    });

    es.onopen = () => {
      this.reconnectDelay = 2000;
      this.onConnected?.();
    };

    es.onerror = () => {
      es.close();
      this.es = null;
      this.onDisconnected?.();
      this.scheduleReconnect();
    };
  }

  disconnect(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.es?.close();
    this.es = null;
  }

  private scheduleReconnect(): void {
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, this.reconnectDelay);
    this.reconnectDelay = Math.min(this.reconnectDelay * 1.5, 30_000);
  }

  private async cmd<T = unknown>(name: string, args?: Record<string, unknown>): Promise<T> {
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (this.token) headers["Authorization"] = `Bearer ${this.token}`;
    const res = await fetch(`${this.baseUrl}/api/cmd/${name}`, {
      method: "POST",
      headers,
      body: args ? JSON.stringify(args) : undefined,
    });
    if (!res.ok) {
      const text = await res.text().catch(() => res.statusText);
      throw new Error(`${name} failed (${res.status}): ${text}`);
    }
    if (res.status === 204 || res.headers.get("content-length") === "0") {
      return undefined as T;
    }
    return res.json() as Promise<T>;
  }

  async listBots(): Promise<BotWire[]> {
    return this.cmd<BotWire[]>("agent_list_bots");
  }

  /**
   * Send a message to a bot. Returns once the server has accepted the turn (202).
   * Streaming response arrives via the SSE connection as `content_delta` / `finished` events.
   */
  async send(bot: string, session: string | undefined, text: string): Promise<void> {
    await this.cmd("agent_send", { project: bot, session, text });
  }

  async cancel(bot: string, session: string): Promise<void> {
    await this.cmd("agent_cancel", { project: bot, session });
  }

  /**
   * Subscribe to a session and get the current snapshot.
   * Call this after navigating to a chat to replay buffered events and get the cursor.
   */
  async subscribe(bot: string, session: string): Promise<{ sequence: number }> {
    return this.cmd("agent_subscribe", { project: bot, session });
  }
}

/** Singleton bridge; configure with initBridge() before first use. */
let _bridge: Bridge | null = null;

export function initBridge(options: BridgeOptions = {}): Bridge {
  _bridge = new Bridge(options);
  return _bridge;
}

export function getBridge(): Bridge {
  if (!_bridge) _bridge = new Bridge();
  return _bridge;
}
