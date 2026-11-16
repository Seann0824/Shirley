import { useCallback, useEffect, useRef, useState } from "react";

import { api, onAgentEvent } from "../api/bridge";
import type { AgentEvent, ItemWire, RenderPart, ToolCallWire } from "../api/types";

/** A view item: the server's `ItemWire` plus a client-only streaming flag on messages. */
export type ConversationItem =
  | { kind: "message"; role: string; content: string; parts: RenderPart[]; streaming?: boolean }
  | { kind: "compact"; content: string }
  | { kind: "tools"; calls: ToolCallWire[] };

const MAIN_SESSION = "main";

function fromSnapshot(items: ItemWire[]): ConversationItem[] {
  return items.map((item) => ({ ...item }));
}

/** Close any assistant message that is still receiving deltas. */
function sealStreaming(items: ConversationItem[]): ConversationItem[] {
  const last = items[items.length - 1];
  if (last?.kind === "message" && last.streaming) {
    return [...items.slice(0, -1), { ...last, streaming: false }];
  }
  return items;
}

function reduce(items: ConversationItem[], event: AgentEvent): ConversationItem[] {
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

/**
 * The bot's one conversation: snapshot on open, then live events layered on top. Events older
 * than the snapshot's cursor are already in it and are skipped.
 */
export function useConversation(bot: string, onTurnFinished?: () => void) {
  const [items, setItems] = useState<ConversationItem[]>([]);
  const [running, setRunning] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const cursor = useRef<number | null>(null);
  const buffered = useRef<AgentEvent[]>([]);
  const finishedCallback = useRef(onTurnFinished);
  finishedCallback.current = onTurnFinished;

  useEffect(() => {
    let cancelled = false;
    cursor.current = null;
    buffered.current = [];
    setItems([]);
    setLoaded(false);
    setError(null);

    const apply = (event: AgentEvent) => {
      setItems((current) => reduce(current, event));
      if (event.type === "finished" || event.type === "error") {
        setRunning(false);
        finishedCallback.current?.();
      }
    };

    const stop = onAgentEvent((event) => {
      if (cancelled) return;
      if (event.bot !== bot || (event.session ?? MAIN_SESSION) !== MAIN_SESSION) return;
      if (cursor.current === null) {
        buffered.current.push(event);
        return;
      }
      if (event.sequence !== undefined && event.sequence <= cursor.current) return;
      apply(event);
    });

    void api
      .subscribe(bot)
      .then((snapshot) => {
        if (cancelled) return;
        setItems(fromSnapshot(snapshot.items));
        setRunning(snapshot.running);
        cursor.current = snapshot.sequence;
        setLoaded(true);
        for (const event of buffered.current) {
          if (event.sequence === undefined || event.sequence > snapshot.sequence) apply(event);
        }
        buffered.current = [];
      })
      .catch((reason: unknown) => {
        if (cancelled) return;
        setError(reason instanceof Error ? reason.message : String(reason));
        setLoaded(true);
      });

    return () => {
      cancelled = true;
      stop();
    };
  }, [bot]);

  const send = useCallback(
    async (text: string, images: string[] = []) => {
      setItems((current) => [
        ...current,
        { kind: "message", role: "user", content: text, parts: [] },
      ]);
      setRunning(true);
      try {
        await api.send(bot, text, images);
      } catch (reason) {
        setRunning(false);
        setItems((current) => [
          ...current,
          {
            kind: "message",
            role: "error",
            content: reason instanceof Error ? reason.message : String(reason),
            parts: [],
          },
        ]);
      }
    },
    [bot],
  );

  const cancel = useCallback(() => api.cancel(bot).catch(() => undefined), [bot]);

  return { items, running, loaded, error, send, cancel };
}
