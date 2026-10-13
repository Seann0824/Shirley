import { AlertCircle, Sparkles } from "lucide-react";
import { useEffect, useRef } from "react";
import { ScrollArea } from "@/ui/scroll-area";
import { cn } from "@/ui/utils";
import type { SkillEntry } from "@/lib/bridge";

/**
 * `/` 技能候选浮层。与 `FileMentionPopover` 同款形态：一列候选 + 键盘选择，
 * 但不带搜索框——query 由输入框本身承载（`/` 之后的文本即查询）。
 */
export function SkillSlashPopover({
  open,
  query,
  results,
  error,
  selectedIndex,
  onSelect,
  onRetry,
}: {
  open: boolean;
  query: string;
  results: SkillEntry[];
  error: string | null;
  selectedIndex: number;
  onSelect: (result: SkillEntry) => void;
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

  const showEmpty = !error && results.length === 0;

  return (
    <div
      role="listbox"
      aria-label="选择技能"
      className="z-50 w-full min-w-0 overflow-hidden rounded-card border border-line bg-surface shadow-lg"
      data-skill-slash-popover
    >
      <ScrollArea
        type="scroll"
        viewportRef={viewportRef}
        viewportClassName="max-h-[min(16rem,45dvh)] overscroll-contain"
      >
        <div className="py-1">
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
                点击重试
              </button>
            </div>
          )}
          {showEmpty && (
            <div className="px-3 py-2 text-body-sm text-muted">
              {query ? "没有匹配的技能" : "当前项目没有可用技能"}
            </div>
          )}
          {!error &&
            results.map((item, index) => {
              const isSelected = index === selectedIndex;
              return (
                <button
                  key={item.name}
                  type="button"
                  role="option"
                  aria-selected={isSelected}
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => onSelect(item)}
                  title={item.description}
                  className={cn(
                    "flex w-full min-w-0 items-center gap-2 px-3 py-1.5 text-left text-body-sm transition-colors",
                    isSelected ? "bg-selected text-ink" : "text-ink hover:bg-quiet",
                  )}
                >
                  <Sparkles className="size-3.5 shrink-0 text-muted" data-icon="inline-start" />
                  <span className="min-w-0 flex-1 truncate">/{item.name}</span>
                  {item.description && (
                    <span className="min-w-0 max-w-[55%] shrink-0 truncate text-caption text-muted">
                      {item.description}
                    </span>
                  )}
                </button>
              );
            })}
        </div>
      </ScrollArea>
    </div>
  );
}
