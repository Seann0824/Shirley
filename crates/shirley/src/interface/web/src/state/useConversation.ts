import { useCallback, useEffect, useRef, useState } from "react";

import { api, onAgentEvent } from "../api/bridge";
import type { AgentEvent, RenderPart } from "../api/types";
import {
  fromSnapshot,
  reduce,
  type ConversationItem,
} from "./conversation";

export type { ConversationItem };

const MAIN_SESSION = "main";

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

    const load = () =>
      api
        .subscribe(bot)
        .then((snapshot) => {
          if (cancelled) return;
          setItems(fromSnapshot(snapshot.items, snapshot.running));
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

    const stop = onAgentEvent((event) => {
      if (cancelled) return;
      if (event.type === "resync") {
        cursor.current = null;
        void load();
        return;
      }
      if (event.bot !== bot || (event.session ?? MAIN_SESSION) !== MAIN_SESSION) return;
      if (cursor.current === null) {
        buffered.current.push(event);
        return;
      }
      if (event.sequence !== undefined && event.sequence <= cursor.current) return;
      apply(event);
    });

    void load();

    return () => {
      cancelled = true;
      stop();
    };
  }, [bot]);

  const send = useCallback(
    async (text: string, images: string[] = [], parts: RenderPart[] = []) => {
      setItems((current) => [...current, { kind: "message", role: "user", content: text, parts }]);
      setRunning(true);
      try {
        // The bubble matched what was typed; the wire still needs something to answer.
        await api.send(bot, text.trim() || (images.length ? "请处理这些附件" : ""), images);
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

  /** Show a client-side problem (e.g. a failed upload) inline, like a server error. */
  const report = useCallback((message: string) => {
    setItems((current) => [...current, { kind: "message", role: "error", content: message, parts: [] }]);
  }, []);

  return { items, running, loaded, error, send, cancel, report };
}
