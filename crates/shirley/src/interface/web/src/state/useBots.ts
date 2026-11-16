import { useCallback, useEffect, useState } from "react";

import { api } from "../api/bridge";
import type { Bot } from "../api/types";

/** Per-device bot list preferences (pin / mute / hide). Not server state on purpose. */
interface BotPrefs {
  pinned: string[];
  muted: string[];
  hidden: string[];
}

const PREFS_KEY = "shirley.botPrefs";

function loadPrefs(): BotPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (raw) return { pinned: [], muted: [], hidden: [], ...(JSON.parse(raw) as Partial<BotPrefs>) };
  } catch {
    // Corrupt prefs are not worth a broken home screen.
  }
  return { pinned: [], muted: [], hidden: [] };
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
      setBots(await api.listBots());
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  }, [prefs]);

  const visible = bots
    .filter((bot) => !prefs.hidden.includes(bot.name))
    .sort((left, right) => {
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
    hide: (name: string) =>
      setPrefs((p) => ({ ...p, hidden: [...p.hidden, name], pinned: p.pinned.filter((n) => n !== name) })),
  };
}
