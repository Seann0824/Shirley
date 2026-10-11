import { File, Folder, Sparkles, X } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "@/ui/utils";
import { CHIP_TOKEN } from "./editor-dom";
import type { FileReference, ReferenceKind } from "./types";

/** chip 行首图标：目录 / 文件 / 技能（`/` 入口）。两类引用共用同一渲染。 */
function iconFor(kind: ReferenceKind) {
  if (kind === "dir") return Folder;
  if (kind === "skill") return Sparkles;
  return File;
}

/**
 * 单个引用 chip（`@` 文件/目录 与 `/` 技能共用）。`onRemove` 省略时为只读展示
 * （用于已发送的用户消息回显）；传入时为可删除（输入框内的引用仍由编辑区按退格整体删除）。
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
  return (
    <span
      className={cn(
        "inline-flex min-w-0 max-w-full items-center gap-1 rounded-control bg-inset px-2 py-0.5 align-middle text-body-sm text-ink",
        className,
      )}
      title={tooltip}
      data-reference-chip
      data-kind={reference.kind}
    >
      <Icon className="size-3 shrink-0 text-muted" data-icon="inline-start" />
      <span className="min-w-0 max-w-40 truncate">
        {reference.kind === "skill" ? `/${reference.name}` : reference.name}
      </span>
      {onRemove && (
        <button
          type="button"
          aria-label={`移除引用：${reference.name}`}
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
 * 把「带占位符的正文」与 references 交错渲染成**内联混排**：chip 落在它在正文里的
 * 原始位置，而不是堆在顶部。第 N 个 `CHIP_TOKEN` ↔ `references[N]`。
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

// lucide 图标路径（与上面的 <File/> / <Folder/> / <Sparkles/> 同源），供 contenteditable
// 里直接构造 DOM 节点用（React 不接管编辑区内部，故手写 outerHTML）。
const FILE_ICON_PATHS = [
  "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z",
  "M14 2v4a2 2 0 0 0 2 2h4",
];
const FOLDER_ICON_PATHS = [
  "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z",
];
// lucide `sparkles`（0.487）的 path 集合，与 `SkillSlashPopover` 的行首图标一致。
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
 * 构造编辑区里的一个内联 chip 元素（`contenteditable=false`）。
 * 样式与 React 版 `FileChip` 对齐，保证输入框内与发送后回显一致。
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
  const paths = iconPathsFor(reference.kind)
    .map((d) => `<path d="${d}"/>`)
    .join("");
  const label = reference.kind === "skill" ? `/${reference.name}` : reference.name;
  span.innerHTML =
    `<svg class="size-3 shrink-0 text-muted" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths}</svg>` +
    `<span class="min-w-0 max-w-40 truncate">${escapeHtml(label)}</span>`;
  return span;
}
