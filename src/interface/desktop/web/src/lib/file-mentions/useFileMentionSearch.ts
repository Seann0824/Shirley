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
  /** 打开浮层（空 query 立即拉一次，不防抖）。 */
  open: () => void;
  setQuery: (query: string) => void;
  close: () => void;
  moveSelection: (delta: number) => void;
  /** 确认当前高亮项，返回被选中的条目（浮层不可确认时返回 null）。 */
  confirmSelection: () => FileReference | null;
  retry: () => void;
};

const SEARCH_DEBOUNCE_MS = 120;

/**
 * `@` 引用的**无头搜索控制器**：只管「候选浮层 + 异步检索」，
 * 不碰任何输入框 DOM。检索走 Rust 侧 `agent_search_files`（应用层），
 * 前端不持有工作区文件状态。
 *
 * 编辑区（`MentionEditor`）在用户敲 `@` 时 `open()`、随输入 `setQuery()`，
 * 选中后把返回的 `FileReference` 插成 inline chip。
 */
export function useFileMentionSearch(): FileMentionSearchState {
  const [popoverOpen, setPopoverOpen] = useState(false);
  const [query, setQueryState] = useState("");
  const [results, setResults] = useState<FileReference[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const searchTimerRef = useRef<number | null>(null);
  const lastQueryRef = useRef("");

  const cancelPendingSearch = useCallback(() => {
    if (searchTimerRef.current) {
      window.clearTimeout(searchTimerRef.current);
      searchTimerRef.current = null;
    }
  }, []);

  const executeSearch = useCallback(async (searchQuery: string) => {
    lastQueryRef.current = searchQuery;
    setLoading(true);
    setError(null);
    try {
      const items = await (await agentBridge()).searchFiles(searchQuery);
      setResults(items);
      setSelectedIndex(0);
    } catch (reason) {
      const message =
        reason instanceof Error && reason.message.trim()
          ? reason.message
          : "加载失败，请稍后重试";
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
    // 空 query 立即拉一次（不防抖），让用户一敲 `@` 就有候选。
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
