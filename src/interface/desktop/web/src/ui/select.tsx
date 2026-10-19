import { Check, ChevronDown } from "lucide-react";
import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { ScrollArea } from "@/ui/scroll-area";
import { cn } from "@/ui/utils";

export type SelectOption<T extends string> = {
  value: T;
  label: string;
  /** Optional trailing hint rendered muted on the right (e.g. a provider name). */
  hint?: string;
  disabled?: boolean;
};

type Placement = "bottom" | "top";

/**
 * A design-system select: a trigger button plus a portalled listbox.
 *
 * Built by composing existing primitives (`ScrollArea`, `lucide` icons, `cn`) instead of
 * a native `<select>` — the native control falls back to the OS popup menu, which ignores
 * our theme and looks foreign next to the rest of the UI. This renders an in-app menu that
 * matches the surface tokens and supports keyboard navigation.
 *
 * Placement is resolved from the trigger's on-screen box: the menu opens below by default
 * and flips above when there is not enough room, matching the height it would need.
 */
export function Select<T extends string>({
  value,
  onChange,
  options,
  label,
  disabled,
  className,
  menuClassName,
  placeholder = "Select…",
}: {
  value: T | "";
  onChange: (next: T) => void;
  options: SelectOption<T>[];
  label: string;
  disabled?: boolean;
  className?: string;
  menuClassName?: string;
  placeholder?: string;
}) {
  const [open, setOpen] = useState(false);
  const [placement, setPlacement] = useState<Placement>("bottom");
  const [activeIndex, setActiveIndex] = useState(-1);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const listId = useId();

  const selected = options.find((option) => option.value === value);

  const close = useCallback((focusTrigger = true) => {
    setOpen(false);
    if (focusTrigger) triggerRef.current?.focus();
  }, []);

  // Position: anchor to the trigger, flip above when it would overflow the viewport.
  const [box, setBox] = useState({ left: 0, top: 0, width: 0, placement: "bottom" as Placement });
  useLayoutEffect(() => {
    if (!open || !triggerRef.current) return;
    const rect = triggerRef.current.getBoundingClientRect();
    const margin = 8;
    const menu = menuRef.current;
    const menuHeight = menu?.getBoundingClientRect().height ?? 0;
    const spaceBelow = window.innerHeight - rect.bottom;
    const next: Placement = spaceBelow < menuHeight + margin && rect.top > menuHeight + margin ? "top" : "bottom";
    setPlacement(next);
    setBox({
      left: rect.left,
      top: next === "bottom" ? rect.bottom + 4 : rect.top - menuHeight - 4,
      width: rect.width,
      placement: next,
    });
  }, [open, options.length]);

  // Outside click / Esc / scroll / resize close the menu (same rules as ContextMenu).
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!triggerRef.current?.contains(target) && !menuRef.current?.contains(target)) {
        setOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        close();
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKey);
    window.addEventListener("resize", () => setOpen(false));
    window.addEventListener("scroll", () => setOpen(false), true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, close]);

  // When opening, highlight the current selection so keyboard nav starts there.
  useEffect(() => {
    if (open) setActiveIndex(options.findIndex((option) => option.value === value));
  }, [open, options, value]);

  const move = (delta: number) => {
    if (options.length === 0) return;
    setActiveIndex((index) => {
      let next = index;
      for (let step = 0; step < options.length; step += 1) {
        next = (next + delta + options.length) % options.length;
        if (!options[next]?.disabled) return next;
      }
      return index;
    });
  };

  const commit = (index: number) => {
    const option = options[index];
    if (!option || option.disabled) return;
    onChange(option.value);
    close();
  };

  const onTriggerKeyDown = (event: React.KeyboardEvent) => {
    if (disabled) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (!open) {
        setOpen(true);
        return;
      }
      if (event.key === "Enter" || event.key === " ") {
        commit(activeIndex);
        return;
      }
      move(event.key === "ArrowDown" ? 1 : -1);
    }
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        role="combobox"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        disabled={disabled}
        onClick={() => setOpen((prev) => !prev)}
        onKeyDown={onTriggerKeyDown}
        className={cn(
          "flex h-9 min-w-[8rem] max-w-[16rem] items-center justify-between gap-2 rounded-control border border-line bg-canvas px-2.5 text-body-sm text-ink transition-colors",
          "hover:bg-quiet focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ink",
          "disabled:pointer-events-none disabled:opacity-50",
          open && "border-line-strong",
          className,
        )}
      >
        <span className={cn("min-w-0 flex-1 truncate text-left", !selected && "text-muted")}>
          {selected ? selected.label : placeholder}
        </span>
        <ChevronDown className={cn("size-4 shrink-0 text-muted transition-transform", open && "rotate-180")} />
      </button>
      {open &&
        createPortal(
          <div
            ref={menuRef}
            className={cn(
              "fixed z-50 overflow-hidden rounded-card border border-line bg-overlay p-1 shadow-overlay",
              menuClassName,
            )}
            style={{ left: box.left, top: box.top, minWidth: Math.max(box.width, 128) }}
          >
            <ScrollArea
              role="listbox"
              id={listId}
              aria-label={label}
              viewportClassName="max-h-64 overscroll-contain"
              style={{ position: "static" }}
            >
              {options.map((option, index) => {
                const isSelected = option.value === value;
                return (
                  <button
                    key={option.value}
                    type="button"
                    role="option"
                    aria-selected={isSelected}
                    disabled={option.disabled}
                    onMouseEnter={() => setActiveIndex(index)}
                    onClick={() => commit(index)}
                    className={cn(
                      "flex w-full items-center justify-between gap-2 rounded-control px-2 py-1.5 text-left text-body-sm text-ink transition-colors",
                      "disabled:pointer-events-none disabled:opacity-45",
                      index === activeIndex && "bg-quiet",
                      isSelected && "bg-selected",
                    )}
                  >
                    <span className="min-w-0 flex-1 truncate">{option.label}</span>
                    {isSelected ? (
                      <Check className="size-4 shrink-0 text-primary" />
                    ) : (
                      option.hint && <span className="shrink-0 text-caption text-muted">{option.hint}</span>
                    )}
                  </button>
                );
              })}
            </ScrollArea>
          </div>,
          document.body,
        )}
    </>
  );
}

export type { ReactNode };
