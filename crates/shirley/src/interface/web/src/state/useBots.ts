import { useCallback, useEffect, useState } from "react";

import { api, onAgentEvent } from "../api/bridge";
import type { Bot } from "../api/types";
import { noteEvent, replyingFromList } from "./replying";

/** Per-device bot list preferences (pin / mute). Not server state on purpose. */
interface BotPrefs {
  pinned: string[];
  muted: string[];
}

const PREFS_KEY = "shirley.botPrefs";

function loadPrefs(): BotPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (raw) {
      // `hidden` (the old "remove", which only hid a bot on this device) is dropped: those bots
      // show again and can now be deleted for real.
      const saved = JSON.parse(raw) as Partial<BotPrefs>;
      return { pinned: saved.pinned ?? [], muted: saved.muted ?? [] };
    }
  } catch {
    // Corrupt prefs are not worth a broken home screen.
  }
  return { pinned: [], muted: [] };
}

function toggle(list: string[], name: string): string[] {
  return list.includes(name) ? list.filter((item) => item !== name) : [...list, name];
}

export function useBots() {
  const [bots, setBots] = useState<Bot[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [prefs, setPrefs] = useState<BotPrefs>(loadPrefs);

  const refresh = useCallback(async () => {
    try {
      const list = await api.listBots();
      setBots(list);
      replyingFromList(list);
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Replies start and end while the list is on screen; after a gap in the stream, ask again.
  useEffect(
    () =>
      onAgentEvent((event) => {
        if (event.type === "resync") void refresh();
        else noteEvent(event);
      }),
    [refresh],
  );

  useEffect(() => {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  }, [prefs]);

  const visible = [...bots].sort((left, right) => {
      const pin = Number(prefs.pinned.includes(right.name)) - Number(prefs.pinned.includes(left.name));
      return pin !== 0 ? pin : 0;
    });

  return {
    bots,
    visible,
    error,
    refresh,
    prefs,
    togglePin: (name: string) => setPrefs((p) => ({ ...p, pinned: toggle(p.pinned, name) })),
    toggleMute: (name: string) => setPrefs((p) => ({ ...p, muted: toggle(p.muted, name) })),
    /** Delete the bot on the server, then forget it here too. */
    remove: async (name: string) => {
      await api.deleteBot(name);
      setPrefs((p) => ({
        pinned: p.pinned.filter((n) => n !== name),
        muted: p.muted.filter((n) => n !== name),
      }));
      await refresh();
    },
  };
}
