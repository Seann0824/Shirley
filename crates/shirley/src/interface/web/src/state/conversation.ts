import type { AgentEvent, ItemWire, RenderPart, ToolCallWire } from "../api/types";

/** A view item: the server's `ItemWire` plus a client-only streaming flag on messages. */
export type ConversationItem =
  | { kind: "message"; role: string; content: string; parts: RenderPart[]; streaming?: boolean }
  | { kind: "compact"; content: string }
  | {
      kind: "tools";
      calls: ToolCallWire[];
      /** Restored from the log: a missing outcome means the call ended, not that it runs. */
      settled?: boolean;
    };

export function fromSnapshot(items: ItemWire[], running = false): ConversationItem[] {
  return items.map((item, index) =>
    item.kind === "tools"
      ? // Only the newest group of a turn that is still going may animate.
        {
          kind: "tools",
          calls: item.calls.map((call) => ({ ...call })),
          settled: !(running && index === items.length - 1),
        }
      : { ...item },
  );
}

/**
 * Mark every tool group finished. A group can only still be running while it is the last thing
 * in the conversation: once anything follows it, or the turn ends, its calls are over -- even if
 * a `tool_finished` event was lost on the way (a lagged or reconnected stream).
 */
function settleTools(items: ConversationItem[]): ConversationItem[] {
  return items.some((item) => item.kind === "tools" && !item.settled)
    ? items.map((item) => (item.kind === "tools" && !item.settled ? { ...item, settled: true } : item))
    : items;
}

/** Close any assistant message that is still receiving deltas, and any tool group before it. */
function sealStreaming(items: ConversationItem[]): ConversationItem[] {
  const last = items[items.length - 1];
  const sealed =
    last?.kind === "message" && last.streaming
      ? [...items.slice(0, -1), { ...last, streaming: false }]
      : items;
  return settleTools(sealed);
}

export function reduce(items: ConversationItem[], event: AgentEvent): ConversationItem[] {
  switch (event.type) {
    case "content_delta": {
      const last = items[items.length - 1];
      if (last?.kind === "message" && last.streaming) {
        return [...items.slice(0, -1), { ...last, content: last.content + event.text }];
      }
      return [
        ...settleTools(items),
        { kind: "message", role: "assistant", content: event.text, parts: [], streaming: true },
      ];
    }
    case "message_added":
      return event.role === "assistant" ? sealStreaming(items) : items;
    case "tool_started": {
      const last0 = items[items.length - 1];
      // Joining the live group keeps it open; anything else starts a new group after a settled one.
      const sealed =
        last0?.kind === "tools" && !last0.settled
          ? items
          : sealStreaming(items);
      const call: ToolCallWire = {
        id: event.call_id,
        name: event.name,
        arguments: event.arguments,
        output: null,
        ok: null,
        parts: [],
      };
      const last = sealed[sealed.length - 1];
      if (last?.kind === "tools" && !last.settled) {
        return [...sealed.slice(0, -1), { kind: "tools", calls: [...last.calls, call] }];
      }
      return [...sealed, { kind: "tools", calls: [call] }];
    }
    case "tool_finished":
      return items.map((item) =>
        item.kind === "tools"
          ? {
              ...item,
              calls: item.calls.map((call) =>
                call.id === event.call_id
                  ? { ...call, output: event.output, ok: event.ok, parts: event.parts ?? [] }
                  : call,
              ),
            }
          : item,
      );
    case "compact":
      return [...sealStreaming(items), { kind: "compact", content: event.content }];
    case "notice":
      return [
        ...sealStreaming(items),
        { kind: "message", role: "notice", content: event.text, parts: [] },
      ];
    case "retrying":
      return [
        ...sealStreaming(items),
        {
          kind: "message",
          role: "notice",
          content: `第 ${event.attempt} 次重试：${event.reason}`,
          parts: [],
        },
      ];
    case "error":
      return [
        ...sealStreaming(items),
        { kind: "message", role: "error", content: event.message, parts: [] },
      ];
    case "finished":
      return sealStreaming(items);
    default:
      return items;
  }
}
