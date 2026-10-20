import { Check, ChevronDown, FolderKanban, MessageSquare, Plus } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { cn } from "@/ui/utils";
import type { ProjectEntry } from "@/lib/bridge";

type AppMode = "chat" | "work";

/**
 * The context selector bar directly below the input, in the new-chat (codex-style empty
 * session) flow.
 *
 * - **Chat / Work**: toggles "whether this is attached to a project" — Chat is not
 *   associated with any project, Work is associated with one.
 * - In **Work**, a **linked-project switcher** (dropdown, opens upward) is offered on the
 *   right. Selecting a project switches to a new empty session under that project (same
 *   semantics as "new session").
 *
 * Used only in the empty-session state; this control disappears after the first message is
 * sent. All switching goes through the caller (`onSelectMode` / `onSelectProject`) down to
 * `newSession`; this component holds no session state.
 */
export function NewChatControls({
  mode,
  projects,
  activeProject,
  onSelectMode,
  onSelectProject,
  onAddProject,
}: {
  mode: AppMode;
  projects: ProjectEntry[];
  activeProject: string | null;
  onSelectMode: (mode: AppMode) => void;
  onSelectProject: (project: string) => void;
  onAddProject: () => void;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const activeName = projects.find((entry) => entry.id === activeProject)?.name ?? "Select a project";

  // Close the project dropdown on click outside / Esc.
  useEffect(() => {
    if (!open) return;
    const onDown = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="new-chat-controls mt-3 flex max-w-full flex-wrap items-center justify-center gap-2 px-3 pb-3">
      {/* Chat / Work segmented toggle */}
      <div
        className="flex items-center gap-0.5 rounded-control bg-inset p-0.5"
        role="tablist"
        aria-label="Mode"
      >
        {(["chat", "work"] as const).map((m) => (
          <button
            key={m}
            type="button"
            role="tab"
            aria-selected={mode === m}
            className={cn(
              "flex items-center gap-1.5 rounded-[6px] px-2.5 py-1 text-caption font-medium transition-colors",
              mode === m ? "bg-canvas text-ink shadow-sm" : "text-muted hover:text-ink",
            )}
            onClick={() => onSelectMode(m)}
          >
            {m === "chat" ? <MessageSquare className="size-3.5" /> : <FolderKanban className="size-3.5" />}
            {m === "chat" ? "Chat" : "Work"}
          </button>
        ))}
      </div>

      {/* Work mode: switch the linked project below the input (menu opens upward). */}
      {mode === "work" && (
        <div ref={rootRef} className="relative">
          <button
            type="button"
            aria-haspopup="menu"
            aria-expanded={open}
            className="flex max-w-[220px] items-center gap-1.5 rounded-control border border-line bg-surface px-2.5 py-1.5 text-caption text-ink transition-colors hover:bg-quiet"
            onClick={() => setOpen((prev) => !prev)}
          >
            <FolderKanban className="size-3.5 shrink-0 text-muted" />
            <span className="min-w-0 truncate">{activeName}</span>
            <ChevronDown className="size-3.5 shrink-0 text-muted" />
          </button>
          {open && (
            <div
              role="menu"
              className="absolute bottom-full left-1/2 z-30 mb-1 max-h-72 w-56 -translate-x-1/2 overflow-y-auto rounded-card border border-line bg-overlay p-1 shadow-overlay"
            >
              {projects.length === 0 && (
                <p className="px-2 py-1.5 text-caption text-muted">No projects yet.</p>
              )}
              {projects.map((entry) => (
                <button
                  key={entry.id}
                  type="button"
                  role="menuitem"
                  className="flex w-full items-center gap-2 rounded-control px-2 py-1.5 text-left text-body-sm text-ink transition-colors hover:bg-quiet"
                  onClick={() => {
                    setOpen(false);
                    if (entry.id !== activeProject) onSelectProject(entry.id);
                  }}
                >
                  <FolderKanban className="size-3.5 shrink-0 text-muted" />
                  <span className="min-w-0 flex-1 truncate">{entry.name}</span>
                  {entry.id === activeProject && <Check className="size-3.5 shrink-0 text-ink" />}
                </button>
              ))}
              <div className="my-1 h-px bg-line" role="separator" />
              <button
                type="button"
                role="menuitem"
                className="flex w-full items-center gap-2 rounded-control px-2 py-1.5 text-left text-body-sm text-ink transition-colors hover:bg-quiet"
                onClick={() => {
                  setOpen(false);
                  onAddProject();
                }}
              >
                <Plus className="size-3.5 shrink-0 text-muted" />
                New project
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
