import { AlertCircle, File, Folder, Loader2, RefreshCw, Search } from "lucide-react";
import { useEffect, useRef } from "react";
import { ScrollArea } from "@/ui/scroll-area";
import { cn } from "@/ui/utils";
import type { FileReference } from "./types";

/**
 * `@` 引用候选浮层。迁移自 shiwen 的 EntityMentionPopover，剥掉了
 * OCR / 删除资料 / 分组（article/space/document…）等 shiwen 定制逻辑，
 * 只留「一个搜索框 + 一列文件/目录结果 + 键盘选择」。
 */
export function FileMentionPopover({
  open,
  query,
  results,
  loading,
  error,
  selectedIndex,
  onSelect,
  onQueryChange,
  onKeyDown,
  onRetry,
}: {
  open: boolean;
  query: string;
  results: FileReference[];
  loading: boolean;
  error: string | null;
  selectedIndex: number;
  onSelect: (result: FileReference) => void;
  onQueryChange: (query: string) => void;
  onKeyDown?: (event: React.KeyboardEvent<HTMLInputElement>) => boolean;
  onRetry: () => void;
}) {
  const viewportRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const viewport = viewportRef.current;
    const selected = viewport?.querySelector<HTMLElement>('[aria-selected="true"]');
    if (!viewport || !selected) return;
    const viewportRect = viewport.getBoundingClientRect();
    const selectedRect = selected.getBoundingClientRect();
    if (selectedRect.top < viewportRect.top) {
      viewport.scrollTop += selectedRect.top - viewportRect.top;
    } else if (selectedRect.bottom > viewportRect.bottom) {
      viewport.scrollTop += selectedRect.bottom - viewportRect.bottom;
    }
  }, [open, results, selectedIndex]);

  if (!open) return null;

  const showEmpty = !loading && !error && results.length === 0;

  return (
    <div
      role="listbox"
      aria-label="引用工作区文件"
      className="z-50 w-full min-w-0 overflow-hidden rounded-card border border-line bg-surface shadow-lg"
      data-file-mention-popover
    >
      <div className="flex items-center gap-2 border-b border-line px-3 py-2">
        <Search className="size-3.5 shrink-0 text-muted" />
        <input
          type="text"
          value={query}
          onChange={(event) => onQueryChange(event.target.value)}
          placeholder="搜索工作区文件…"
          className="w-full bg-transparent text-body-sm text-ink outline-none placeholder:text-muted"
          aria-label="搜索要引用的文件"
          onKeyDown={(event) => {
            if (event.key === "Enter") event.preventDefault();
            event.stopPropagation();
            onKeyDown?.(event);
          }}
        />
      </div>
      <ScrollArea
        type="auto"
        viewportRef={viewportRef}
        viewportClassName="max-h-[min(16rem,45dvh)] overscroll-contain"
      >
        <div className="py-1">
          {loading && (
            <div className="flex items-center gap-2 px-3 py-2 text-body-sm text-muted">
              <Loader2 className="size-3.5 animate-spin" />
              搜索中…
            </div>
          )}
          {error && (
            <div className="flex flex-col gap-2 px-3 py-2">
              <div className="flex items-center gap-1.5 text-body-sm text-danger-ink">
                <AlertCircle className="size-3.5" />
                {error}
              </div>
              <button
                type="button"
                onClick={onRetry}
                className="flex w-fit items-center gap-1 rounded bg-quiet px-2 py-1 text-label text-ink hover:bg-overlay"
              >
                <RefreshCw className="size-3" />
                点击重试
              </button>
            </div>
          )}
          {showEmpty && (
            <div className="px-3 py-2 text-body-sm text-muted">没有找到匹配的文件</div>
          )}
          {!loading &&
            !error &&
            results.map((item, index) => {
              const isSelected = index === selectedIndex;
              const Icon = item.kind === "dir" ? Folder : File;
              return (
                <button
                  key={item.path}
                  type="button"
                  role="option"
                  aria-selected={isSelected}
                  onClick={() => onSelect(item)}
                  title={item.path}
                  className={cn(
                    "flex w-full min-w-0 items-center gap-2 px-3 py-1.5 text-left text-body-sm transition-colors",
                    isSelected ? "bg-selected text-ink" : "text-ink hover:bg-quiet",
                  )}
                >
                  <Icon className="size-3.5 shrink-0 text-muted" data-icon="inline-start" />
                  <span className="min-w-0 flex-1 truncate">{item.name}</span>
                  <span className="min-w-0 max-w-[55%] shrink-0 truncate text-caption text-muted">
                    {item.path}
                  </span>
                </button>
              );
            })}
        </div>
      </ScrollArea>
    </div>
  );
}
