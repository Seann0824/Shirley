import { AtSign, ListChecks, Paperclip, Plus } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/ui/button";
import { cn } from "@/ui/utils";

export type ComposerTaskEntry = {
  completed: number;
  total: number;
  /** Whether the ledger card is currently open. */
  open: boolean;
  onToggle: () => void;
};

/**
 * The composer's single "add something" entry.
 *
 * Everything that used to be a separate button (attach a picture, reference a workspace file,
 * open the task list) now hangs off one `+`. The bar stays quiet — one affordance on the
 * left, `model · dictation · send` on the right — instead of a row of icons whose meaning has
 * to be learned one by one. The layout follows the composer people already know; the styling
 * is this project's own.
 */
export function ComposerAddMenu({
  onAttachFiles,
  onReference,
  tasks,
  className,
}: {
  onAttachFiles: () => void;
  onReference: () => void;
  /** Omitted when this session has no task ledger yet. */
  tasks?: ComposerTaskEntry;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

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

  const pick = (action: () => void) => {
    setOpen(false);
    action();
  };

  return (
    <div ref={rootRef} className={cn("relative", className)}>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        aria-label="Add attachment or reference"
        aria-haspopup="menu"
        aria-expanded={open}
        title="Add"
        className={cn("rounded-full text-muted hover:text-ink", open && "bg-quiet text-ink")}
        onClick={() => setOpen((prev) => !prev)}
      >
        <Plus />
      </Button>
      {open && (
        <div
          role="menu"
          aria-label="Add"
          className="absolute bottom-[calc(100%+0.5rem)] left-0 z-30 w-64 rounded-card border border-line bg-surface p-1 shadow-lg"
        >
          <MenuItem
            icon={<Paperclip className="size-4 shrink-0" aria-hidden="true" />}
            label="Images and files"
            detail="Attach a picture, PDF or clip"
            onClick={() => pick(onAttachFiles)}
          />
          <MenuItem
            icon={<AtSign className="size-4 shrink-0" aria-hidden="true" />}
            label="Reference a file"
            detail="Point at a path in this project"
            onClick={() => pick(onReference)}
          />
          {tasks && (
            <MenuItem
              icon={<ListChecks className="size-4 shrink-0" aria-hidden="true" />}
              label="Task list"
              detail={
                tasks.total > 0 ? `${tasks.completed}/${tasks.total} done` : "Nothing planned yet"
              }
              active={tasks.open}
              onClick={() => pick(tasks.onToggle)}
            />
          )}
        </div>
      )}
    </div>
  );
}

function MenuItem({
  icon,
  label,
  detail,
  active,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  detail: string;
  active?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      className={cn(
        "flex w-full items-center gap-2.5 rounded-control px-3 py-2 text-left text-body-sm text-ink transition-colors hover:bg-quiet",
        active && "bg-selected",
      )}
      onClick={onClick}
    >
      <span className="text-muted">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <span className="shrink-0 text-caption text-muted">{detail}</span>
    </button>
  );
}
