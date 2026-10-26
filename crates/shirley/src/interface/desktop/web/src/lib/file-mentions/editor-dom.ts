import type { FileReference } from "./types";

/**
 * Placeholder in the body representing a reference chip (U+FFFC, OBJECT REPLACEMENT CHARACTER).
 *
 * In the contenteditable editor, a chip is a `contenteditable=false` inline element; when
 * serialized to plain text it becomes this invisible character, so "text + references" is
 * still a single **ordered string**: the Nth placeholder <-> references[N]. The placeholders
 * are stripped before sending to the model, and the references still travel through the
 * existing references array (Rust `compose_prompt` unchanged).
 */
export const CHIP_TOKEN = "\uFFFC";

/**
 * Zero-width space (U+200B) used as a trailing sentinel. A chip is an atomic
 * `contenteditable=false` node; when it lands at the **end** of the editor, the caret at
 * the "after the chip" DOM boundary has no text node to attach to, so the browser
 * **does not render the caret** (it looks like the caret vanished until another character
 * is typed). So append a zero-width-space text node after a trailing chip to give the caret
 * a renderable anchor; serialization and offset computation strip it uniformly, so the user
 * never notices.
 */
const ZWSP = "\u200B";

/** Serialize a DOM subtree into "plain text with placeholders". */
function serializeNode(node: Node): string {
  if (node.nodeType === Node.TEXT_NODE) return (node.textContent ?? "").split(ZWSP).join("");
  if (node.nodeType === Node.ELEMENT_NODE) {
    const el = node as HTMLElement;
    if (el.dataset.chip === "true") return CHIP_TOKEN;
    if (el.tagName === "BR") return "\n";
    let out = "";
    el.childNodes.forEach((child) => {
      out += serializeNode(child);
    });
    return out;
  }
  return "";
}

export function serializeEditor(root: HTMLElement): string {
  let out = "";
  root.childNodes.forEach((node) => {
    out += serializeNode(node);
  });
  return out;
}

/** Read all chips from the editor in DOM order (for reconciling references after a chip is deleted via the keyboard). */
export function readChips(root: HTMLElement): FileReference[] {
  return Array.from(root.querySelectorAll<HTMLElement>("[data-chip='true']")).map((el) => {
    const kind =
      el.dataset.kind === "dir"
        ? "dir"
        : el.dataset.kind === "skill"
          ? "skill"
          : el.dataset.kind === "image"
            ? "image"
            : el.dataset.kind === "media"
              ? "media"
              : "file";
    const reference: FileReference = {
      path: el.dataset.path ?? "",
      name: el.dataset.name ?? "",
      kind,
    };
    if (el.dataset.description) reference.description = el.dataset.description;
    // A library chip carries its artifact id (not bytes): keep it in the DOM so the chip
    // survives an editor rebuild (Backspace / re-render) without losing the payload.
    if (el.dataset.artifactId) reference.artifactId = el.dataset.artifactId;
    if (el.dataset.mime) reference.mime = el.dataset.mime;
    if (el.dataset.bytes) reference.bytes = Number(el.dataset.bytes);
    // Keep the chip's thumbnail across a rebuild: the preview URL (an object URL for an
    // uploaded image, a `data:` URL for a pasted one) is written onto the element by
    // `createChipElement` and read back here.
    if (el.dataset.previewSrc) reference.dataUrl = el.dataset.previewSrc;
    return reference;
  });
}

/** Rebuild the editor content from "placeholder text + references" (called only when the structure changes). */
export function buildEditorContent(
  root: HTMLElement,
  text: string,
  references: FileReference[],
  createChip: (reference: FileReference, index: number) => HTMLElement,
): void {
  root.replaceChildren();
  const parts = text.split(CHIP_TOKEN);
  let refIndex = 0;
  parts.forEach((part, index) => {
    if (part) root.appendChild(document.createTextNode(part));
    if (index < parts.length - 1) {
      const reference = references[refIndex];
      refIndex += 1;
      if (reference) root.appendChild(createChip(reference, refIndex - 1));
    }
  });
  // A trailing chip: append a zero-width-space sentinel so the "after the chip" caret has an anchor (otherwise it is not rendered).
  if (text.endsWith(CHIP_TOKEN)) root.appendChild(document.createTextNode(ZWSP));
}

/** The caret offset in the "serialized text" (in UTF-16 code units). */
export function caretOffset(root: HTMLElement): number {
  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0) return serializeEditor(root).length;
  const range = selection.getRangeAt(0);
  if (!root.contains(range.endContainer)) return serializeEditor(root).length;
  const before = range.cloneRange();
  before.selectNodeContents(root);
  before.setEnd(range.endContainer, range.endOffset);
  const holder = document.createElement("div");
  holder.appendChild(before.cloneContents());
  return serializeEditor(holder).length;
}

/** Resolve a "serialized text offset" into a DOM position (node, offset). */
export function resolveOffset(root: HTMLElement, offset: number): { node: Node; offset: number } {
  let remaining = offset;
  let result: { node: Node; offset: number } | null = null;

  const walk = (node: Node): boolean => {
    for (let i = 0; i < node.childNodes.length; i += 1) {
      const child = node.childNodes[i];
      if (child.nodeType === Node.TEXT_NODE) {
        const raw = child.textContent ?? "";
        const length = raw.split(ZWSP).join("").length;
        if (remaining <= length) {
          // The text node may contain the zero-width sentinel: map the "serialized offset" back to the real DOM offset.
          const offsetInNode = remaining + (raw.startsWith(ZWSP) ? 1 : 0);
          result = { node: child, offset: offsetInNode };
          return true;
        }
        remaining -= length;
      } else if (child.nodeType === Node.ELEMENT_NODE) {
        const el = child as HTMLElement;
        if (el.dataset.chip === "true") {
          if (remaining <= 0) {
            // Before the chip: prefer landing at the end of the previous text node (otherwise the caret is not rendered at the start/end of a line).
            const prev = node.childNodes[i - 1];
            if (prev && prev.nodeType === Node.TEXT_NODE) {
              result = { node: prev, offset: (prev.textContent ?? "").length };
            } else {
              result = { node, offset: i };
            }
            return true;
          }
          remaining -= 1;
          if (remaining <= 0) {
            // After the chip: prefer landing at the start of the next text node (or after
            // the zero-width char for the sentinel node), giving the caret a renderable text
            // anchor rather than stopping at the boundary between the chip and the text.
            const next = node.childNodes[i + 1];
            if (next && next.nodeType === Node.TEXT_NODE) {
              const raw = next.textContent ?? "";
              result = { node: next, offset: raw.startsWith(ZWSP) ? 1 : 0 };
            } else {
              result = { node, offset: i + 1 };
            }
            return true;
          }
        } else if (el.tagName === "BR") {
          if (remaining <= 0) {
            result = { node, offset: i };
            return true;
          }
          remaining -= 1;
        } else if (walk(el)) {
          return true;
        }
      }
    }
    return false;
  };

  walk(root);
  return result ?? { node: root, offset: root.childNodes.length };
}

/** Place the caret at the given offset in the "serialized text". */
export function setCaretAt(root: HTMLElement, offset: number): void {
  const { node, offset: domOffset } = resolveOffset(root, offset);
  const range = document.createRange();
  range.setStart(node, domOffset);
  range.collapse(true);
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
}

/** The start offset of a chip element in the "serialized text" (i.e. the position of its placeholder). */
export function chipOffset(root: HTMLElement, chip: Node): number | null {
  if (!root.contains(chip)) return null;
  const before = document.createRange();
  before.selectNodeContents(root);
  before.setEndBefore(chip);
  const holder = document.createElement("div");
  holder.appendChild(before.cloneContents());
  return serializeEditor(holder).length;
}

/** Walk up from any node to find the chip it belongs to (for click positioning); returns null if not inside a chip. */
export function closestChip(root: HTMLElement, node: Node | null): HTMLElement | null {
  if (!node) return null;
  const el =
    node.nodeType === Node.ELEMENT_NODE ? (node as HTMLElement) : node.parentElement;
  const chip = el?.closest<HTMLElement>("[data-chip='true']") ?? null;
  return chip && root.contains(chip) ? chip : null;
}

/** The number of CHIP_TOKENs in the body. */
export function countTokens(text: string): number {
  return text.split(CHIP_TOKEN).length - 1;
}

/** Strip all reference placeholders to get the plain text actually handed to the model. */
export function stripTokens(text: string): string {
  return text.split(CHIP_TOKEN).join("");
}

/** Insert plain text at the caret (used for paste / Shift+Enter line breaks). */
export function insertTextAtCaret(root: HTMLElement, text: string): void {
  const selection = window.getSelection();
  if (
    !selection ||
    selection.rangeCount === 0 ||
    !root.contains(selection.getRangeAt(0).startContainer)
  ) {
    root.appendChild(document.createTextNode(text));
    setCaretAt(root, serializeEditor(root).length);
    return;
  }
  const range = selection.getRangeAt(0);
  range.deleteContents();
  const node = document.createTextNode(text);
  range.insertNode(node);
  range.setStartAfter(node);
  range.collapse(true);
  selection.removeAllRanges();
  selection.addRange(range);
  ensureTrailingSentinel(root);
}

/** Append a zero-width-space sentinel when a chip is at the end (see the `ZWSP` note). */
export function ensureTrailingSentinel(root: HTMLElement): void {
  const last = root.lastChild;
  if (
    last &&
    last.nodeType === Node.ELEMENT_NODE &&
    (last as HTMLElement).dataset.chip === "true"
  ) {
    root.appendChild(document.createTextNode(ZWSP));
  }
}
