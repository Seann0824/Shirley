import { Check, ChevronDown, Settings2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/ui/button";
import { ScrollArea } from "@/ui/scroll-area";
import { cn } from "@/ui/utils";
import type { ModelEntry } from "@/lib/bridge";

/**
 * Model switcher: sits next to the send button.
 *
 * Just a thin UI over "model catalog + hot switch" — both the catalog and the switch are
 * provided by the Rust side (`ModelCatalog` / `Agent::set_model`); the frontend holds no
 * model state.
 */
export function ModelSelector({
  project,
  current,
  onSelect,
  className,
}: {
  project: string;
  current: string;
  onSelect: (value: string) => void;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [entries, setEntries] = useState<ModelEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let disposed = false;
    const load = async () => {
      try {
        const { agentBridge } = await import("@/lib/bridge");
        const entries = await (await agentBridge()).listModels(project);
        if (!disposed) setEntries(entries);
      } catch { /* Opening the menu reports connection errors. */ }
    };
    if (current.startsWith("provider:")) void load();
    window.addEventListener("model-providers-updated", load);
    return () => { disposed = true; window.removeEventListener("model-providers-updated", load); };
  }, [project, current]);
  const selected = entries.find((entry) => entry.value === current);
  const currentLabel = selected ? `${selected.provider} / ${selected.label}` : current.startsWith("provider:") ? "Select provider model" : current;

  // Close on click outside the component / Esc.
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
    setError("");
    try {
      const { agentBridge } = await import("@/lib/bridge");
      setEntries(await (await agentBridge()).listModels(project));
    } catch (cause) {
      setEntries([]);
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div data-model-selector ref={rootRef} className={cn("relative", className)}>
      <Button
        type="button"
        variant="ghost"
        size="xs"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label="Switch model"
        className="max-w-[12rem] gap-1 px-2"
        onClick={() => void toggle()}
      >
        <span className="truncate">{currentLabel || "No model selected"}</span>
        <ChevronDown className="size-3.5 shrink-0" />
      </Button>
      {open && (
        <ScrollArea
          role="listbox"
          aria-label="Available models"
          className="absolute bottom-[calc(100%+0.5rem)] right-0 z-20 w-64 rounded-card border border-line bg-surface shadow-lg"
          // The max height must be set on the **viewport**: Radix's viewport is `h-full`,
          // so if the cap were on the Root (auto height), the viewport would grow with the
          // content, never overflow, and the scrollbar would never appear (models cut off).
          // Kept consistent with `FileMentionPopover`.
          viewportClassName="max-h-72 overscroll-contain"
          // Radix ScrollArea's Root injects an inline `position: relative`, and inline
          // styles outrank the `absolute` class — without overriding it, the element falls
          // back into normal flow and stretches the input box. Explicitly restore absolute
          // positioning here.
          style={{ position: "absolute" }}
        >
          <div className="p-1">
            {loading && <p className="px-3 py-2 text-caption text-muted">Loading...</p>}
            {!loading && error && <p className="px-3 py-2 text-caption text-danger-ink">Failed to load model list: {error}</p>}
            {!loading && !error && entries.length === 0 && (
              <p className="px-3 py-2 text-caption text-muted">No models available.</p>
            )}
            {!loading &&
              entries.map((entry, index) => {
                const active = entry.value === current;
                return (
                  <div key={entry.value}>
                  {(index === 0 || entries[index - 1].provider !== entry.provider) && <p className="px-3 pt-2 pb-1 text-caption font-medium text-muted">{entry.provider}</p>}
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
                      onSelect(entry.value);
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
                  </div>
                );
              })}
            <button type="button" className="mt-1 flex w-full items-center gap-2 border-t border-line px-3 py-2 text-body-sm text-muted hover:bg-selected hover:text-ink" onClick={() => { setOpen(false); window.dispatchEvent(new Event("open-model-providers")); }}><Settings2 className="size-4" />Manage model providers</button>
          </div>
        </ScrollArea>
      )}
    </div>
  );
}
