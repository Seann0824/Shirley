import { useCallback, useEffect, useRef, useState } from "react";
import { agentBridge } from "@/lib/bridge";
import type { FileReference } from "./types";

export type FileMentionSearchState = {
  popoverOpen: boolean;
  query: string;
  results: FileReference[];
  loading: boolean;
  error: string | null;
  selectedIndex: number;
  /** Opens the popover (an empty query fetches immediately, no debounce). */
  open: () => void;
  setQuery: (query: string) => void;
  close: () => void;
  moveSelection: (delta: number) => void;
  /** Confirms the currently highlighted item and returns the selected entry (null when the popover cannot confirm). */
  confirmSelection: () => FileReference | null;
  retry: () => void;
};

const SEARCH_DEBOUNCE_MS = 120;

/**
 * Headless search controller for `@` references: only handles "candidate popover + async
 * search", never touches any input DOM. Search goes through the Rust side's
 * `agent_search_files` (application layer); the frontend holds no workspace file state.
 *
 * The editor (`MentionEditor`) calls `open()` when the user types `@` and `setQuery()` as
 * the input changes; on selection the returned `FileReference` is inserted as an inline chip.
 */
export function useFileMentionSearch(project: string | null): FileMentionSearchState {
  const [popoverOpen, setPopoverOpen] = useState(false);
  const [query, setQueryState] = useState("");
  const [results, setResults] = useState<FileReference[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const searchTimerRef = useRef<number | null>(null);
  const lastQueryRef = useRef("");
  // Current project (workspace): search must carry the project id. A ref avoids stale
  // closures so switching projects does not require rebuilding the search controller.
  const projectRef = useRef(project);
  projectRef.current = project;

  const cancelPendingSearch = useCallback(() => {
    if (searchTimerRef.current) {
      window.clearTimeout(searchTimerRef.current);
      searchTimerRef.current = null;
    }
  }, []);

  const executeSearch = useCallback(async (searchQuery: string) => {
    lastQueryRef.current = searchQuery;
    const projectId = projectRef.current;
    // No foreground project (workspace): no files to search, so return empty and skip the backend.
    if (!projectId) {
      setResults([]);
      setError(null);
      setLoading(false);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const items = await (await agentBridge()).searchFiles(projectId, searchQuery);
      setResults(items);
      setSelectedIndex(0);
    } catch (reason) {
      const message =
        reason instanceof Error && reason.message.trim()
          ? reason.message
          : "Failed to load, please try again later";
      setError(message);
      setResults([]);
    } finally {
      setLoading(false);
    }
  }, []);

  const debouncedSearch = useCallback(
    (searchQuery: string) => {
      lastQueryRef.current = searchQuery;
      if (searchTimerRef.current) window.clearTimeout(searchTimerRef.current);
      setSelectedIndex(0);
      setLoading(true);
      setError(null);
      searchTimerRef.current = window.setTimeout(() => {
        searchTimerRef.current = null;
        void executeSearch(searchQuery);
      }, SEARCH_DEBOUNCE_MS);
    },
    [executeSearch],
  );

  const open = useCallback(() => {
    cancelPendingSearch();
    lastQueryRef.current = "";
    setPopoverOpen(true);
    setQueryState("");
    setResults([]);
    setError(null);
    setSelectedIndex(0);
    // An empty query fetches immediately (no debounce) so candidates appear the moment `@` is typed.
    void executeSearch("");
  }, [cancelPendingSearch, executeSearch]);

  const close = useCallback(() => {
    cancelPendingSearch();
    setPopoverOpen(false);
    setQueryState("");
    setResults([]);
    setError(null);
    setSelectedIndex(0);
  }, [cancelPendingSearch]);

  const setQuery = useCallback(
    (nextQuery: string) => {
      setQueryState(nextQuery);
      debouncedSearch(nextQuery);
    },
    [debouncedSearch],
  );

  const moveSelection = useCallback(
    (delta: number) => {
      setSelectedIndex((current) => {
        if (results.length === 0) return 0;
        return (current + delta + results.length) % results.length;
      });
    },
    [results.length],
  );

  const confirmSelection = useCallback((): FileReference | null => {
    if (!loading && results[selectedIndex]) return results[selectedIndex];
    return null;
  }, [results, selectedIndex, loading]);

  const retry = useCallback(() => {
    void executeSearch(lastQueryRef.current);
  }, [executeSearch]);

  useEffect(() => cancelPendingSearch, [cancelPendingSearch]);

  return {
    popoverOpen,
    query,
    results,
    loading,
    error,
    selectedIndex,
    open,
    setQuery,
    close,
    moveSelection,
    confirmSelection,
    retry,
  };
}
