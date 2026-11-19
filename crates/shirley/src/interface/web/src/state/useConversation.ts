import { useCallback, useEffect, useRef, useState } from "react";

import { api, onAgentEvent } from "../api/bridge";
import type { AgentEvent, RenderPart } from "../api/types";
import {
  appendUser,
  fromSnapshot,
  reduce,
  type ConversationItem,
} from "./conversation";

export type { ConversationItem };

const MAIN_SESSION = "main";
/** Items fetched on open and per scroll-up: enough to fill a phone screen a few times over. */
export const PAGE_SIZE = 40;

/**
 * The bot's one conversation: the newest page on open, then live events layered on top. Events
 * older than the snapshot's cursor are already in it and are skipped. Older pages are fetched on
 * demand (`loadOlder`) as the user scrolls up.
 */
export function useConversation(bot: string, onTurnFinished?: () => void) {
  const [items, setItems] = useState<ConversationItem[]>([]);
  // Turns asked for and not yet finished. Sending mid-reply interrupts it: the old turn's
  // `finished` arrives after the new turn was asked for, and must not clear the new one.
  const [outstanding, setOutstanding] = useState(0);
  const running = outstanding > 0;
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Index of `items[0]` in the whole conversation: above 0, older items wait on the server.
  const [start, setStart] = useState(0);
  const [loadingOlder, setLoadingOlder] = useState(false);
  // Bumped on every (re)load, so a page that arrives after a reload is dropped, not prepended.
  const generation = useRef(0);
  const olderInFlight = useRef(false);
  const cursor = useRef<number | null>(null);
  const reload = useRef<() => Promise<void>>(async () => undefined);
  const buffered = useRef<AgentEvent[]>([]);
  const finishedCallback = useRef(onTurnFinished);
  finishedCallback.current = onTurnFinished;

  useEffect(() => {
    let cancelled = false;
    cursor.current = null;
    buffered.current = [];
    setItems([]);
    setStart(0);
    setLoaded(false);
    setError(null);

    const apply = (event: AgentEvent) => {
      setItems((current) => reduce(current, event));
      if (event.type === "finished" || event.type === "error") {
        setOutstanding((count) => Math.max(0, count - 1));
        finishedCallback.current?.();
      }
    };

    const load = () => {
      generation.current += 1;
      olderInFlight.current = false;
      setLoadingOlder(false);
      return api
        .subscribe(bot, PAGE_SIZE)
        .then((snapshot) => {
          if (cancelled) return;
          setItems(fromSnapshot(snapshot.items, snapshot.running));
          setStart(snapshot.start);
          setOutstanding(snapshot.running ? 1 : 0);
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
    };

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

    reload.current = load;
    void load();

    return () => {
      cancelled = true;
      stop();
    };
  }, [bot]);

  const send = useCallback(
    async (text: string, images: string[] = [], parts: RenderPart[] = []) => {
      setItems((current) => appendUser(current, text, parts));
      setOutstanding((count) => count + 1);
      try {
        // The bubble matched what was typed; the wire still needs something to answer.
        await api.send(bot, text.trim() || (images.length ? "请处理这些附件" : ""), images);
      } catch (reason) {
        setOutstanding((count) => Math.max(0, count - 1));
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

  /** Fetch the page before the oldest item shown and put it on top. */
  const loadOlder = useCallback(async () => {
    if (start <= 0 || olderInFlight.current) return;
    olderInFlight.current = true;
    setLoadingOlder(true);
    const asked = generation.current;
    try {
      const page = await api.history(bot, start, PAGE_SIZE);
      if (asked !== generation.current) return;
      setItems((current) => [...fromSnapshot(page.items), ...current]);
      setStart(page.start);
    } catch {
      // Leave `start` as it is: the next scroll to the top asks again.
    } finally {
      if (asked === generation.current) {
        olderInFlight.current = false;
        setLoadingOlder(false);
      }
    }
  }, [bot, start]);

  const cancel = useCallback(() => api.cancel(bot).catch(() => undefined), [bot]);

  /** Take back the latest message (stopping its reply); resolves to its text, to edit. */
  const recall = useCallback(async () => {
    const { text } = await api.recall(bot);
    cursor.current = null;
    await reload.current();
    return text;
  }, [bot]);

  /** Show a client-side problem (e.g. a failed upload) inline, like a server error. */
  const report = useCallback((message: string) => {
    setItems((current) => [...current, { kind: "message", role: "error", content: message, parts: [] }]);
  }, []);

  return {
    items,
    running,
    loaded,
    error,
    send,
    cancel,
    recall,
    report,
    start,
    hasOlder: start > 0,
    loadingOlder,
    loadOlder,
  };
}
