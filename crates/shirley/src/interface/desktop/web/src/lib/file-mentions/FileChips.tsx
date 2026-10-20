import { File, Folder, Sparkles, X } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "@/ui/utils";
import { CHIP_TOKEN } from "./editor-dom";
import type { FileReference, ReferenceKind } from "./types";

/** Chip leading icon: directory / file / skill (`/` entry). Both reference kinds share the same rendering. */
function iconFor(kind: ReferenceKind) {
  if (kind === "dir") return Folder;
  if (kind === "skill") return Sparkles;
  return File;
}

/**
 * A single reference chip (shared by `@` file/dir and `/` skill). When `onRemove` is
 * omitted it is read-only (for echoing a sent user message); when provided it is
 * removable (references inside the input box are still deleted as a whole with Backspace
 * by the editor).
 */
export function FileChip({
  reference,
  onRemove,
  className,
}: {
  reference: FileReference;
  onRemove?: (path: string) => void;
  className?: string;
}) {
  const Icon = iconFor(reference.kind);
  const tooltip = reference.description
    ? `${reference.name} — ${reference.description}`
    : reference.path;
  const isImage = reference.kind === "image";
  const imageSrc = isImage ? (reference.dataUrl ?? reference.path) : "";
  return (
    <span
      className={cn(
        "inline-flex min-w-0 max-w-full items-center gap-1 rounded-control bg-inset px-2 py-0.5 align-middle text-body-sm text-ink",
        className,
      )}
      title={tooltip}
      data-reference-chip
      data-kind={reference.kind}
      // Marker only: `ImageHoverPreview` reads the nested <img>.src, so the (potentially
      // large) data URL is not duplicated into an attribute.
      data-image-preview={isImage ? "true" : undefined}
    >
      {isImage ? (
        <img
          src={imageSrc}
          alt={reference.name}
          className="size-4 shrink-0 rounded-[3px] object-cover"
          data-icon="inline-start"
        />
      ) : (
        <Icon className="size-3 shrink-0 text-muted" data-icon="inline-start" />
      )}
      <span className="min-w-0 max-w-40 truncate">
        {reference.kind === "skill" ? `/${reference.name}` : reference.name}
      </span>
      {onRemove && (
        <button
          type="button"
          aria-label={`Remove reference: ${reference.name}`}
          onClick={() => onRemove(reference.path)}
          className={cn(
            "shrink-0 rounded-full p-0.5 text-muted transition-colors",
            "hover:bg-selected hover:text-ink",
          )}
        >
          <X className="size-3" />
        </button>
      )}
    </span>
  );
}

/**
 * Interleaves "body text with placeholders" and references into an **inline mix**: the
 * chip lands at its original position in the body text instead of being stacked on top.
 * The Nth `CHIP_TOKEN` <-> `references[N]`.
 */
export function InlineReferences({
  content,
  references,
  className,
}: {
  content: string;
  references: FileReference[];
  className?: string;
}) {
  const nodes: ReactNode[] = [];
  const parts = content.split(CHIP_TOKEN);
  let refIndex = 0;
  parts.forEach((part, index) => {
    if (part) nodes.push(<span key={`t${index}`}>{part}</span>);
    if (index < parts.length - 1) {
      const reference = references[refIndex];
      refIndex += 1;
      if (reference) nodes.push(<FileChip key={`c${index}`} reference={reference} />);
    }
  });
  return <span className={cn("whitespace-pre-wrap", className)}>{nodes}</span>;
}

// lucide icon paths (same source as <File/> / <Folder/> / <Sparkles/> above), for
// directly constructing DOM nodes inside contenteditable (React does not manage the
// editor interior, so outerHTML is written by hand).
const FILE_ICON_PATHS = [
  "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z",
  "M14 2v4a2 2 0 0 0 2 2h4",
];
const FOLDER_ICON_PATHS = [
  "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z",
];
// The path set for lucide `sparkles` (0.487), matching the leading icon in `SkillSlashPopover`.
const SKILL_ICON_PATHS = [
  "M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z",
  "M20 3v4",
  "M22 5h-4",
  "M4 17v2",
  "M5 18H3",
];

function iconPathsFor(kind: ReferenceKind): string[] {
  if (kind === "dir") return FOLDER_ICON_PATHS;
  if (kind === "skill") return SKILL_ICON_PATHS;
  return FILE_ICON_PATHS;
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/**
 * Builds an inline chip element inside the editor (`contenteditable=false`).
 * Styling matches the React `FileChip` so the input box and the sent echo look the same.
 */
export function createChipElement(reference: FileReference): HTMLElement {
  const span = document.createElement("span");
  span.className =
    "inline-flex min-w-0 max-w-full select-none items-center gap-1 rounded-control bg-inset px-2 py-0.5 align-middle text-body-sm text-ink";
  span.contentEditable = "false";
  span.dataset.chip = "true";
  span.dataset.path = reference.path;
  span.dataset.name = reference.name;
  span.dataset.kind = reference.kind;
  if (reference.description) span.dataset.description = reference.description;
  span.title = reference.description
    ? `${reference.name} — ${reference.description}`
    : reference.path;
  const label = reference.kind === "skill" ? `/${reference.name}` : reference.name;
  if (reference.kind === "image") {
    // Pasted image: render a thumbnail instead of an icon; `data-image-preview` marks the
    // chip for the global hover preview (which reads this <img>.src).
    const src = reference.dataUrl ?? reference.path;
    span.dataset.imagePreview = "true";
    span.innerHTML =
      `<img src="${escapeHtml(src)}" alt="${escapeHtml(reference.name)}" class="size-4 shrink-0 rounded-[3px] object-cover" />` +
      `<span class="min-w-0 max-w-40 truncate">${escapeHtml(label)}</span>`;
    return span;
  }
  const paths = iconPathsFor(reference.kind)
    .map((d) => `<path d="${d}"/>`)
    .join("");
  span.innerHTML =
    `<svg class="size-3 shrink-0 text-muted" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths}</svg>` +
    `<span class="min-w-0 max-w-40 truncate">${escapeHtml(label)}</span>`;
  return span;
}
