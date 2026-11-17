import { useCallback, useEffect, useState } from "react";

import { api } from "../api/bridge";
import type { PoolTask } from "../api/types";

const POLL_MS = 5000;

/** The platform task pool. Polls while mounted; every mutation returns the whole list. */
export function useTasks(poll = true) {
  const [tasks, setTasks] = useState<PoolTask[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const settle = useCallback((promise: Promise<PoolTask[]>) =>
    promise
      .then((list) => {
        setTasks(list);
        setError(null);
      })
      .catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))
      .finally(() => setLoaded(true)),
  []);

  const refresh = useCallback(() => settle(api.poolList()), [settle]);

  useEffect(() => {
    void refresh();
    if (!poll) return;
    const timer = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(timer);
  }, [refresh, poll]);

  return {
    tasks,
    loaded,
    error,
    refresh,
    /** Queue a task and resolve to the bot the platform gave it (`null` on failure). */
    enqueue: async (brief: string): Promise<string | null> => {
      try {
        const list = await api.poolEnqueue(brief);
        setTasks(list);
        setError(null);
        const newest = list.reduce<PoolTask | null>(
          (best, task) => (!best || task.createdMs > best.createdMs ? task : best),
          null,
        );
        return newest?.bot ?? null;
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
        return null;
      }
    },
    accept: (id: string) => settle(api.poolAccept(id)),
    reject: (id: string, note: string) => settle(api.poolReject(id, note)),
    note: (id: string, note: string) => settle(api.poolNote(id, note)),
    retry: (id: string) => settle(api.poolRetry(id)),
    cancel: (id: string) => settle(api.poolCancel(id)),
    remove: (id: string) => settle(api.poolRemove(id)),
  };
}
