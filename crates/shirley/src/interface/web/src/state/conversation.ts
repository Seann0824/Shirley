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

/** Close any assistant message that is still receiving deltas. */
function sealStreaming(items: ConversationItem[]): ConversationItem[] {
  const last = items[items.length - 1];
  if (last?.kind === "message" && last.streaming) {
    return [...items.slice(0, -1), { ...last, streaming: false }];
  }
  return items;
}

export function reduce(items: ConversationItem[], event: AgentEvent): ConversationItem[] {
  switch (event.type) {
    case "content_delta": {
      const last = items[items.length - 1];
      if (last?.kind === "message" && last.streaming) {
        return [...items.slice(0, -1), { ...last, content: last.content + event.text }];
      }
      return [
        ...items,
        { kind: "message", role: "assistant", content: event.text, parts: [], streaming: true },
      ];
    }
    case "message_added":
      return event.role === "assistant" ? sealStreaming(items) : items;
    case "tool_started": {
      const sealed = sealStreaming(items);
      const call: ToolCallWire = {
        id: event.call_id,
        name: event.name,
        arguments: event.arguments,
        output: null,
        ok: null,
        parts: [],
      };
      const last = sealed[sealed.length - 1];
      if (last?.kind === "tools") {
        return [...sealed.slice(0, -1), { kind: "tools", calls: [...last.calls, call] }];
      }
      return [...sealed, { kind: "tools", calls: [call] }];
    }
    case "tool_finished":
      return items.map((item) =>
        item.kind === "tools"
          ? {
              kind: "tools",
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
