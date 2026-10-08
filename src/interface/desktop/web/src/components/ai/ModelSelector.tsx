import { Check, ChevronDown } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/ui/button";
import { cn } from "@/ui/utils";
import type { ModelEntry } from "@/lib/bridge";

/**
 * 模型切换选择器：贴在发送按钮旁边。
 *
 * 只是「模型目录 + 热切换」的薄 UI——目录与切换都由 Rust 侧
 * (`ModelCatalog` / `Agent::set_model`) 提供，前端不持有模型状态。
 */
export function ModelSelector({
  current,
  onSelect,
  className,
}: {
  current: string;
  onSelect: (value: string) => void;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [entries, setEntries] = useState<ModelEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  // 点击组件外部 / Esc 关闭。
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  const toggle = async () => {
    const next = !open;
    setOpen(next);
    if (!next) return;
    setLoading(true);
    try {
      const { agentBridge } = await import("@/lib/bridge");
      setEntries(await (await agentBridge()).listModels());
    } catch {
      setEntries([]);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div ref={rootRef} className={cn("relative", className)}>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label="切换模型"
        className="max-w-[12rem] gap-1 px-2 text-muted"
        onClick={() => void toggle()}
      >
        <span className="truncate">{current || "未选择模型"}</span>
        <ChevronDown className="size-3.5 shrink-0" />
      </Button>
      {open && (
        <div
          role="listbox"
          aria-label="可选模型"
          className="absolute bottom-[calc(100%+0.5rem)] right-0 z-20 max-h-72 w-64 overflow-y-auto rounded-card border border-line bg-surface p-1 shadow-lg"
        >
          {loading && <p className="px-3 py-2 text-caption text-muted">加载中…</p>}
          {!loading && entries.length === 0 && (
            <p className="px-3 py-2 text-caption text-muted">没有可用的模型。</p>
          )}
          {!loading &&
            entries.map((entry) => {
              const active = entry.value === current;
              return (
                <button
                  key={entry.value}
                  type="button"
                  role="option"
                  aria-selected={active}
                  className={cn(
                    "flex w-full items-center justify-between gap-2 rounded-control px-3 py-2 text-left text-body-sm text-ink transition-colors",
                    "hover:bg-quiet",
                    active && "bg-selected",
                  )}
                  onClick={() => {
                    setOpen(false);
                    if (!active) onSelect(entry.value);
                  }}
                >
                  <span className="min-w-0 flex-1 truncate">{entry.label}</span>
                  {active ? (
                    <Check className="size-4 shrink-0 text-primary" />
                  ) : (
                    entry.provider && (
                      <span className="shrink-0 text-caption text-muted">{entry.provider}</span>
                    )
                  )}
                </button>
              );
            })}
        </div>
      )}
    </div>
  );
}
