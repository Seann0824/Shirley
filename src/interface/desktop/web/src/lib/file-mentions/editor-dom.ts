import type { FileReference } from "./types";

/**
 * 正文里代表一个引用 chip 的占位符（U+FFFC，OBJECT REPLACEMENT CHARACTER）。
 *
 * contenteditable 编辑区里，chip 是一个 `contenteditable=false` 的内联元素；
 * 序列化成纯文本时它被写成这个不可见字符，于是「文本 + 引用」仍是一条**有序的
 * 字符串**：第 N 个占位符 ↔ references[N]。发送给模型前把占位符剥掉，引用仍走
 * 既有的 references 数组（Rust `compose_prompt` 不变）。
 */
export const CHIP_TOKEN = "\uFFFC";

/**
 * 末尾哨兵用的零宽空格（U+200B）。chip 是 `contenteditable=false` 的原子节点，
 * 当它落在编辑区**末尾**时，光标停在「chip 之后」这个 DOM 边界上没有可依附的文本
 * 节点，浏览器**不渲染光标**（看起来像光标消失，直到再敲一个字符才出现）。
 * 因此在末尾 chip 后追加一个零宽空格文本节点，给光标一个可渲染的落点；序列化与
 * 偏移计算统一把它剥掉，用户完全感知不到。
 */
const ZWSP = "\u200B";

/** 序列化一段 DOM 为「带占位符的纯文本」。 */
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

/** 从编辑区按 DOM 顺序读出所有 chip（用于「键盘删掉 chip」后的引用对账）。 */
export function readChips(root: HTMLElement): FileReference[] {
  return Array.from(root.querySelectorAll<HTMLElement>("[data-chip='true']")).map((el) => {
    const kind =
      el.dataset.kind === "dir" ? "dir" : el.dataset.kind === "skill" ? "skill" : "file";
    const reference: FileReference = {
      path: el.dataset.path ?? "",
      name: el.dataset.name ?? "",
      kind,
    };
    if (el.dataset.description) reference.description = el.dataset.description;
    return reference;
  });
}

/** 用「带占位符的文本 + references」重建编辑区内容（仅结构变化时调用）。 */
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
  // 末尾是 chip：补一个零宽空格哨兵，让「chip 之后」的光标有落点（否则不渲染）。
  if (text.endsWith(CHIP_TOKEN)) root.appendChild(document.createTextNode(ZWSP));
}

/** 光标在「序列化文本」中的偏移（UTF-16 码元数）。 */
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

/** 把「序列化文本偏移」解析成 DOM 位置 (node, offset)。 */
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
          // 文本节点里可能含零宽哨兵：把「序列化偏移」映射回真实 DOM 偏移。
          const offsetInNode = remaining + (raw.startsWith(ZWSP) ? 1 : 0);
          result = { node: child, offset: offsetInNode };
          return true;
        }
        remaining -= length;
      } else if (child.nodeType === Node.ELEMENT_NODE) {
        const el = child as HTMLElement;
        if (el.dataset.chip === "true") {
          if (remaining <= 0) {
            // chip 之前：优先落进前一个文本节点末尾（否则行首/行尾不渲染光标）。
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
            // chip 之后：优先落进后一个文本节点开头（哨兵节点则在零宽字符之后），
            // 让光标有可渲染的文本落点，而不是停在 chip 与文本之间的边界上。
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

/** 把光标放到「序列化文本」的指定偏移处。 */
export function setCaretAt(root: HTMLElement, offset: number): void {
  const { node, offset: domOffset } = resolveOffset(root, offset);
  const range = document.createRange();
  range.setStart(node, domOffset);
  range.collapse(true);
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
}

/** 某个 chip 元素在「序列化文本」里的起始偏移（即其占位符的位置）。 */
export function chipOffset(root: HTMLElement, chip: Node): number | null {
  if (!root.contains(chip)) return null;
  const before = document.createRange();
  before.selectNodeContents(root);
  before.setEndBefore(chip);
  const holder = document.createElement("div");
  holder.appendChild(before.cloneContents());
  return serializeEditor(holder).length;
}

/** 从任意节点向上找到它所属的 chip（点击定位用）；不在 chip 内返回 null。 */
export function closestChip(root: HTMLElement, node: Node | null): HTMLElement | null {
  if (!node) return null;
  const el =
    node.nodeType === Node.ELEMENT_NODE ? (node as HTMLElement) : node.parentElement;
  const chip = el?.closest<HTMLElement>("[data-chip='true']") ?? null;
  return chip && root.contains(chip) ? chip : null;
}

/** 正文里 CHIP_TOKEN 的个数。 */
export function countTokens(text: string): number {
  return text.split(CHIP_TOKEN).length - 1;
}

/** 剥掉所有引用占位符，得到真正交给模型的纯文本。 */
export function stripTokens(text: string): string {
  return text.split(CHIP_TOKEN).join("");
}

/** 在光标处插入纯文本（用于粘贴 / Shift+Enter 换行）。 */
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

/** 末尾是 chip 时补一个零宽空格哨兵（见 `ZWSP` 说明）。 */
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
