import { useCallback, useEffect, useRef, useState } from "react";
import { agentBridge } from "@/lib/bridge";
import type { FileReference } from "./types";

export type FileMentionState = {
  references: FileReference[];
  popoverOpen: boolean;
  query: string;
  results: FileReference[];
  loading: boolean;
  error: string | null;
  selectedIndex: number;
  textareaRef: React.RefObject<HTMLTextAreaElement>;
  openPicker: () => void;
  closePopover: () => void;
  setQuery: (query: string) => void;
  selectResult: (result: FileReference) => void;
  addReference: (reference: FileReference) => void;
  removeReference: (path: string) => void;
  clearReferences: () => void;
  moveSelection: (delta: number) => void;
  confirmSelection: () => boolean;
  handleValueChange: (value: string) => void;
  handleKeyDown: (event: React.KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => boolean;
};

const SEARCH_DEBOUNCE_MS = 120;

function sameReference(a: FileReference, b: FileReference) {
  return a.path === b.path;
}

/**
 * `@` 文件引用：输入框里敲 `@` 触发候选浮层，选中后把 `@query` 从正文里抹掉、
 * 变成一个引用 chip。检索走 Rust 侧 `agent_search_files`（应用层），
 * 前端不持有工作区文件状态。
 */
export function useFileMentions(
  value: string,
  onValueChange: (value: string) => void,
): FileMentionState {
  const [references, setReferences] = useState<FileReference[]>([]);
  const [popoverOpen, setPopoverOpen] = useState(false);
  const [query, setQueryState] = useState("");
  const [results, setResults] = useState<FileReference[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const mentionStartRef = useRef<number>(-1);
  const searchTimerRef = useRef<number | null>(null);
  const lastQueryRef = useRef("");

  const cancelPendingSearch = useCallback(() => {
    if (searchTimerRef.current) {
      window.clearTimeout(searchTimerRef.current);
      searchTimerRef.current = null;
    }
  }, []);

  const closePopover = useCallback(() => {
    cancelPendingSearch();
    setPopoverOpen(false);
    setQueryState("");
    setResults([]);
    setError(null);
    setSelectedIndex(0);
    mentionStartRef.current = -1;
  }, [cancelPendingSearch]);

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

  const openPopover = useCallback(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;
    const cursor = textarea.selectionStart ?? value.length;
    mentionStartRef.current = cursor - 1;
    lastQueryRef.current = "";
    setPopoverOpen(true);
    setQueryState("");
    setResults([]);
    setError(null);
    setSelectedIndex(0);
    // 空 query 立即拉一次（不防抖），让用户一敲 `@` 就有候选。
    void executeSearch("");
  }, [value, executeSearch]);

  const openPicker = useCallback(() => {
    cancelPendingSearch();
    mentionStartRef.current = -1;
    lastQueryRef.current = "";
    setPopoverOpen(true);
    setQueryState("");
    setResults([]);
    setError(null);
    setSelectedIndex(0);
    void executeSearch("");
  }, [cancelPendingSearch, executeSearch]);

  const setQuery = useCallback(
    (nextQuery: string) => {
      setQueryState(nextQuery);
      debouncedSearch(nextQuery);
    },
    [debouncedSearch],
  );

  useEffect(() => cancelPendingSearch, [cancelPendingSearch]);

  const handleValueChange = useCallback(
    (next: string) => {
      onValueChange(next);
      const cursor = textareaRef.current?.selectionStart ?? next.length;
      if (popoverOpen) {
        const start = mentionStartRef.current;
        const query = next.slice(start + 1, cursor);
        if (start < 0 || cursor <= start || next[start] !== "@" || /\s/.test(query)) {
          closePopover();
        } else {
          setQuery(query);
        }
      } else if (next[cursor - 1] === "@") {
        openPopover();
      }
    },
    [onValueChange, popoverOpen, closePopover, setQuery, openPopover],
  );

  const insertReference = useCallback(
    (result: FileReference) => {
      const textarea = textareaRef.current;
      const start =
        mentionStartRef.current >= 0
          ? mentionStartRef.current
          : (textarea?.selectionStart ?? value.length);
      const before = value.slice(0, Math.max(0, start));
      const cursor = textarea?.selectionStart ?? value.length;
      const after = value.slice(cursor);
      // 把 `@query` 从正文里抹掉，改成引用 chip。
      onValueChange(`${before}${after}`);
      setReferences((current) =>
        current.some((reference) => sameReference(reference, result))
          ? current
          : [...current, result],
      );
      closePopover();
      requestAnimationFrame(() => {
        const el = textareaRef.current;
        if (el) {
          const position = before.length;
          el.focus();
          el.setSelectionRange(position, position);
        }
      });
    },
    [value, onValueChange, closePopover],
  );

  const selectResult = useCallback(
    (result: FileReference) => insertReference(result),
    [insertReference],
  );

  const addReference = useCallback((reference: FileReference) => {
    setReferences((current) =>
      current.some((item) => sameReference(item, reference)) ? current : [...current, reference],
    );
  }, []);

  const removeReference = useCallback((path: string) => {
    setReferences((current) => current.filter((reference) => reference.path !== path));
  }, []);

  const clearReferences = useCallback(() => setReferences([]), []);

  const moveSelection = useCallback(
    (delta: number) => {
      setSelectedIndex((current) => {
        if (results.length === 0) return 0;
        return (current + delta + results.length) % results.length;
      });
    },
    [results.length],
  );

  const confirmSelection = useCallback(() => {
    if (!loading && results[selectedIndex]) {
      insertReference(results[selectedIndex]);
      return true;
    }
    return false;
  }, [results, selectedIndex, insertReference, loading]);

  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>): boolean => {
      if (!popoverOpen || event.nativeEvent.isComposing || event.keyCode === 229) return false;
      if (event.key === "ArrowDown") {
        event.preventDefault();
        moveSelection(1);
        return true;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        moveSelection(-1);
        return true;
      }
      if (event.key === "Enter") event.preventDefault();
      if ((event.key === "Enter" || event.key === "Tab") && !loading && results.length > 0) {
        event.preventDefault();
        confirmSelection();
        return true;
      }
      if (event.key === "Enter") return true;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        closePopover();
        return true;
      }
      return false;
    },
    [popoverOpen, moveSelection, confirmSelection, closePopover, results.length, loading],
  );

  return {
    references,
    popoverOpen,
    query,
    results,
    loading,
    error,
    selectedIndex,
    textareaRef,
    openPicker,
    closePopover,
    setQuery,
    selectResult,
    addReference,
    removeReference,
    clearReferences,
    moveSelection,
    confirmSelection,
    handleValueChange,
    handleKeyDown,
  };
}
