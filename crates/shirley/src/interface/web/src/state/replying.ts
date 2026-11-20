import { useEffect, useState } from "react";

import type { AgentEvent, Bot } from "../api/types";

// Which bots are answering in their conversation right now, for the bot list. The server's bot
// list says so when it loads; the event stream keeps it current in between, so the list changes
// the moment a reply starts or ends, whichever screen (or device) sent the message.

const MAIN_SESSION = "main";

/** Events that only happen while a turn runs. */
const LIVE = new Set(["message_added", "content_delta", "tool_started", "tool_finished", "retrying", "compaction_started"]);

let replying = new Set<string>();
const listeners = new Set<(bots: Set<string>) => void>();

function publish(next: Set<string>) {
  replying = next;
  listeners.forEach((listener) => listener(next));
}

/** The server's word, from the bot list. */
export function replyingFromList(bots: Bot[]): void {
  publish(new Set(bots.filter((bot) => bot.running).map((bot) => bot.name)));
}

/** A message just went out: the reply has started, even before its first word arrives. */
export function markReplying(bot: string): void {
  if (!replying.has(bot)) publish(new Set([...replying, bot]));
}

/** The server says the bot is not answering (its `finished` may never have arrived here). */
export function markIdle(bot: string): void {
  if (replying.has(bot)) publish(new Set([...replying].filter((name) => name !== bot)));
}

export function noteEvent(event: AgentEvent): void {
  if (!event.bot || (event.session ?? MAIN_SESSION) !== MAIN_SESSION) return;
  const bot = event.bot;
  if (event.type === "finished" || event.type === "error") {
    markIdle(bot);
  } else if (LIVE.has(event.type)) {
    markReplying(bot);
  }
}

/** The current set, outside React (tests). */
export function replyingSnapshot(): Set<string> {
  return replying;
}

export function useReplying(): Set<string> {
  const [bots, setBots] = useState(replying);
  useEffect(() => {
    listeners.add(setBots);
    setBots(replying);
    return () => {
      listeners.delete(setBots);
    };
  }, []);
  return bots;
}
