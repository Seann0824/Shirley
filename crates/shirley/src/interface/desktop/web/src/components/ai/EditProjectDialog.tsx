import { Folder, FolderPlus, Loader2, X } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { Button } from "@/ui/button";
import { cn } from "@/ui/utils";
import type { ProjectEntry } from "@/lib/bridge";

/**
 * "Edit project" dialog: rename a project and manage its **source folders**.
 *
 * A project may span several local checkouts; exactly one of them is the **primary** folder.
 * The primary folder is the agent working dir + session-storage key, so changing it moves the
 * project's sessions (the backend rebuilds the runtime). The rest are additional roots that the
 * file tools / `@` search resolve against.
 *
 * Layout follows the reference product (name field on top, "Source folders" list with a remove
 * cross, "Add folder", and a footer with a destructive "Remove local project" on the left and
 * Cancel / Save on the right) but uses this project's own design tokens.
 */
export function EditProjectDialog({
  project,
  onCancel,
  onSave,
  onPickFolder,
  onRemove,
}: {
  project: ProjectEntry;
  onCancel: () => void;
  onSave: (changes: { name: string; primary: string; folders: string[] }) => Promise<void>;
  /** Open the native folder picker, returning the chosen absolute path (null if cancelled). */
  onPickFolder: () => Promise<string | null>;
  /** "Remove local project": archive the project (does not delete on-disk data). */
  onRemove: () => void;
}) {
  const titleId = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const [name, setName] = useState(project.name);
  const [folders, setFolders] = useState<string[]>(() => [
    project.path,
    ...(project.extra_paths ?? []),
  ]);
  const [primary, setPrimary] = useState(project.path);
  const [picking, setPicking] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (!element.open) element.showModal();
    return () => {
      if (element.open) element.close();
    };
  }, []);

  const addFolder = async () => {
    setError("");
    setPicking(true);
    try {
      const picked = await onPickFolder();
      if (!picked) return;
      setFolders((prev) => (prev.includes(picked) ? prev : [...prev, picked]));
      // The first folder added to a project with no folders becomes primary automatically;
      // otherwise keep the current primary.
      setPrimary((prev) => (prev && prev.length > 0 ? prev : picked));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPicking(false);
    }
  };

  const removeFolder = (folder: string) => {
    if (folders.length <= 1) {
      setError("A project needs at least one source folder.");
      return;
    }
    const next = folders.filter((item) => item !== folder);
    setFolders(next);
    if (primary === folder) setPrimary(next[0]);
  };

  const save = async () => {
    const trimmed = name.trim();
    if (!trimmed) {
      setError("Project name cannot be empty.");
      return;
    }
    if (!folders.includes(primary)) {
      setError("The primary folder must be one of the source folders.");
      return;
    }
    setError("");
    setSaving(true);
    try {
      await onSave({ name: trimmed, primary, folders });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  };

  return (
    <dialog
      ref={dialog}
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault();
        onCancel();
      }}
      className="m-auto w-[min(30rem,calc(100vw-1.5rem))] overflow-hidden rounded-sheet border border-line bg-canvas p-0 text-ink shadow-sheet backdrop:bg-black/40"
    >
      <div className="flex flex-col">
        <div className="flex items-center justify-between px-5 pt-4 pb-2">
          <h2 id={titleId} className="text-body font-medium">
            Edit project
          </h2>
          <button
            type="button"
            aria-label="Close"
            onClick={onCancel}
            className="rounded-control p-1.5 text-muted hover:bg-selected hover:text-ink"
          >
            <X className="size-4" />
          </button>
        </div>

        <div className="flex flex-col gap-4 px-5 pb-4">
          {/* Project name: leading folder glyph + editable input. */}
          <div className="flex items-center gap-2 rounded-control border border-line bg-canvas px-3 py-2">
            <Folder className="size-4 shrink-0 text-muted" />
            <input
              autoFocus
              value={name}
              onChange={(event) => setName(event.target.value)}
              aria-label="Project name"
              placeholder="Project name"
              className="min-w-0 flex-1 bg-transparent text-body-sm text-ink outline-none placeholder:text-muted"
            />
          </div>

          {/* Source folders: one row per folder, primary marked, remove cross on the right. */}
          <div className="flex flex-col gap-1.5">
            <p className="text-caption font-medium text-muted">Source folders</p>
            <div className="flex flex-col gap-1.5">
              {folders.map((folder) => {
                const isPrimary = folder === primary;
                return (
                  <div
                    key={folder}
                    className="flex items-center gap-2 rounded-control border border-line bg-surface px-3 py-2"
                  >
                    <Folder className="size-4 shrink-0 text-muted" />
                    <button
                      type="button"
                      onClick={() => setPrimary(folder)}
                      title={isPrimary ? "Primary folder" : "Set as primary folder"}
                      className="min-w-0 flex-1 text-left"
                    >
                      <span className="block truncate text-body-sm text-ink">{folder}</span>
                      <span
                        className={cn(
                          "text-caption",
                          isPrimary ? "text-ink" : "text-muted",
                        )}
                      >
                        {isPrimary ? "Primary folder" : "Click to set as primary"}
                      </span>
                    </button>
                    <button
                      type="button"
                      aria-label={`Remove ${folder}`}
                      onClick={() => removeFolder(folder)}
                      disabled={folders.length <= 1}
                      className="shrink-0 rounded-control p-1 text-muted hover:bg-selected hover:text-ink disabled:opacity-40"
                    >
                      <X className="size-3.5" />
                    </button>
                  </div>
                );
              })}
            </div>
            <button
              type="button"
              onClick={() => void addFolder()}
              disabled={picking}
              className="flex items-center gap-2 self-start rounded-control px-2 py-1.5 text-body-sm text-muted transition-colors hover:bg-quiet hover:text-ink disabled:opacity-50"
            >
              {picking ? <Loader2 className="size-3.5 animate-spin" /> : <FolderPlus className="size-3.5" />}
              Add folder
            </button>
          </div>

          {error && <p className="text-caption text-danger-ink">{error}</p>}
        </div>

        {/* Footer: destructive remove on the left, Cancel / Save on the right. */}
        <div className="flex items-center justify-between gap-2 border-t border-line px-5 py-3">
          <Button variant="danger" size="sm" onClick={onRemove}>
            Remove local project
          </Button>
          <div className="flex items-center gap-2">
            <Button variant="ghost" size="sm" onClick={onCancel}>
              Cancel
            </Button>
            <Button variant="primary" size="sm" onClick={() => void save()} disabled={saving}>
              {saving ? <Loader2 className="size-3.5 animate-spin" /> : null}
              Save
            </Button>
          </div>
        </div>
      </div>
    </dialog>
  );
}
