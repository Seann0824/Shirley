import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { cn } from "@/ui/utils";
import { shouldSendChatOnEnter } from "@/lib/chat-input";
import { createChipElement } from "./FileChips";
import {
  buildEditorContent,
  caretOffset,
  CHIP_TOKEN,
  chipOffset,
  closestChip,
  countTokens,
  insertTextAtCaret,
  readChips,
  serializeEditor,
  setCaretAt,
} from "./editor-dom";
import type { FileMentionSearchState } from "./useFileMentionSearch";
import type { SkillSlashState } from "../slash/useSkillSlashSearch";
import type { FileReference } from "./types";
import type { SkillEntry } from "@/lib/bridge";

export type MentionEditorHandle = {
  focus: () => void;
  /** Insert a file/directory as a chip at the caret (called by the external "+" button). */
  insertReference: (reference: FileReference) => void;
  /** Replace the `/` skill candidate with a skill chip (called by clicking an item in `SkillSlashPopover`). */
  insertSkill: (skill: SkillEntry) => void;
  /** Insert a pasted-image chip at the caret (data URL payload). */
  insertImage: (reference: FileReference) => void;
  /** Open the `@` reference picker from the external "+" button (empty query, immediate fetch). */
  openMention: () => void;
};

/** Nearest `/` start before the caret (no whitespace in between); returns -1 if none. */
function slashQueryStart(text: string, caret: number): number {
  for (let i = caret - 1; i >= 0; i -= 1) {
    const ch = text[i];
    if (ch === "/") return i;
    if (/\s/.test(ch)) return -1;
  }
  return -1;
}

/** Keep only as many references as there are placeholders actually present in the body (guards against inconsistent external input). */
function deriveReferences(text: string, references: FileReference[]): FileReference[] {
  const count = countTokens(text);
  if (references.length === count) return references;
  return references.slice(0, count);
}

/**
/** Read a pasted image file into a base64 `data:` URL (the chip / send payload shape). */
function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(typeof reader.result === "string" ? reader.result : "");
    reader.onerror = () => reject(reader.error ?? new Error("failed to read pasted image"));
    reader.readAsDataURL(file);
  });
}

/**
 * `@`-reference rich input (contenteditable). A reference chip is an **inline node** that
 * flows with the text on the same line; the body is serialized into a single ordered
 * string of "text + U+FFFC placeholders", where placeholder order <-> references order.
 *
 * Key constraint: **do not re-render the editor-area DOM while editing** (otherwise the
 * caret is lost). React only renders the root `<div>`; the inner content is maintained
 * entirely imperatively by this component, and is fully rebuilt only on "external reset /
 * insert / delete chip", restoring the caret with `pendingCaret`. Ordinary typing relies on
 * the browser's native editing; `onInput` only serializes and reports.
 */
export const MentionEditor = forwardRef<
  MentionEditorHandle,
  {
    id: string;
    label: string;
    value: string;
    references: FileReference[];
    placeholder: string;
    disabled?: boolean;
    autoFocus?: boolean;
    className?: string;
    search: FileMentionSearchState;
    /** `/` skill-candidate controller (same as `@`; the popover is rendered by the caller). */
    slash: SkillSlashState;
    onChange: (value: string, references: FileReference[]) => void;
    onSubmit: () => void;
  }
>(function MentionEditor(
  { id, label, value, references, placeholder, disabled, autoFocus, className, search, slash, onChange, onSubmit },
  ref,
) {
  const rootRef = useRef<HTMLDivElement>(null);
  const refsRef = useRef<FileReference[]>(references);
  const lastCommittedRef = useRef<string | null>(null);
  // The `@` reference currently being edited: queryStart = offset of `@` in the text (-1 means it came from the "+" button file picker).
  const mentionRef = useRef<{ queryStart: number } | null>(null);
  const pendingCaretRef = useRef<number | null>(null);
  // Latest edit command (called by the native `beforeinput` listener registered once at mount, to avoid stale closures).
  const inputRef = useRef<() => void>(() => {});
  const [empty, setEmpty] = useState(value.length === 0);

  const emit = useCallback(
    (text: string, refs: FileReference[]) => {
      refsRef.current = refs;
      lastCommittedRef.current = text;
      setEmpty(text.length === 0);
      onChange(text, refs);
    },
    [onChange],
  );

  const rebuild = useCallback(
    (text: string, refs: FileReference[], caret: number) => {
      const root = rootRef.current;
      if (!root) return;
      refsRef.current = refs;
      buildEditorContent(root, text, refs, createChipElement);
      lastCommittedRef.current = text;
      setEmpty(text.length === 0);
      setCaretAt(root, caret);
    },
    [],
  );

  // Rebuild when the external value changes (clear / rewind restore). Our own input is already synced, so skip it to avoid losing the caret on re-render.
  useEffect(() => {
    if (value === lastCommittedRef.current) return;
    const root = rootRef.current;
    if (!root) return;
    const refs = deriveReferences(value, references);
    rebuild(value, refs, value.length);
    onChange(value, refs);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value, references]);

  useEffect(() => {
    if (autoFocus) rootRef.current?.focus({ preventScroll: true });
  }, [autoFocus]);

  const syncMention = useCallback(
    (text: string, caret: number, chipCount: number) => {
      if (mentionRef.current) {
        const start = mentionRef.current.queryStart;
        const query = start >= 0 ? text.slice(start + 1, caret) : "";
        if (start < 0 || caret <= start || text[start] !== "@" || /\s/.test(query)) {
          mentionRef.current = null;
          search.close();
        } else {
          search.setQuery(query);
        }
      } else if (text[caret - 1] === "@") {
        mentionRef.current = { queryStart: caret - 1 };
        void chipCount;
        search.open();
      }
    },
    [search],
  );

  const handleInput = useCallback(() => {
    const root = rootRef.current;
    if (!root) return;
    const text = serializeEditor(root);
    let refs = refsRef.current;

    // A chip was deleted via the keyboard (the browser deletes a contenteditable=false node as a whole) -> reconcile the references.
    const chipCount = countTokens(text);
    if (chipCount !== refs.length) {
      if (chipCount < refs.length) {
        const paths = readChips(root).map((chip) => chip.path);
        refs = refs.filter((reference) => paths.includes(reference.path));
      } else {
        refs = refs.slice(0, chipCount);
      }
      emit(text, refs);
      const caret = caretOffset(root);
      syncMention(text, caret, chipCount);
      slash.sync(text);
      return;
    }

    emit(text, refs);
    const caret = caretOffset(root);
    syncMention(text, caret, chipCount);
    slash.sync(text);
  }, [emit, syncMention, slash]);

  /** Insert a reference chip at the caret without touching the `@` mention state (used for pasted images). */
  const insertReferenceAtCaret = useCallback(
    (reference: FileReference) => {
      const root = rootRef.current;
      if (!root) return;
      const text = serializeEditor(root);
      const caret = caretOffset(root);
      const before = text.slice(0, caret);
      const after = text.slice(caret);
      const tokenIndex = countTokens(before);
      const refs = refsRef.current.slice();
      refs.splice(tokenIndex, 0, reference);
      const next = `${before}${CHIP_TOKEN}${after}`;
      rebuild(next, refs, (before + CHIP_TOKEN).length);
      emit(next, refs);
    },
    [rebuild, emit],
  );

  const handleSelect = useCallback(
    (result: FileReference) => {
      const root = rootRef.current;
      if (!root) return;
      const text = serializeEditor(root);
      const caret = caretOffset(root);
      const info = mentionRef.current;
      const from = info && info.queryStart >= 0 ? info.queryStart : caret;
      const before = text.slice(0, from);
      const after = text.slice(caret);
      const tokenIndex = countTokens(before);
      const refs = refsRef.current.slice();
      refs.splice(tokenIndex, 0, result);
      const next = `${before}${CHIP_TOKEN}${after}`;
      mentionRef.current = null;
      search.close();
      rebuild(next, refs, (before + CHIP_TOKEN).length);
      emit(next, refs);
    },
    [rebuild, emit, search],
  );

  // Confirm a skill candidate: replace the current `/query` fragment with a **skill chip**
  // (the same kind of inline node as an `@` file). Arguments (the `/skill` args) can be
  // typed directly after the chip; on Enter submit, `App::send` uses the chip to decide
  // "which skill to activate".
  const handleSkillSelect = useCallback(
    (skill: SkillEntry) => {
      const root = rootRef.current;
      if (!root) return;
      const text = serializeEditor(root);
      const caret = caretOffset(root);
      const start = slashQueryStart(text, caret);
      const from = start >= 0 ? start : caret;
      const before = text.slice(0, from);
      const after = text.slice(caret);
      const reference: FileReference = {
        path: skill.name,
        name: skill.name,
        kind: "skill",
        description: skill.description,
      };
      const tokenIndex = countTokens(before);
      const refs = refsRef.current.slice();
      refs.splice(tokenIndex, 0, reference);
      const next = `${before}${CHIP_TOKEN}${after}`;
      slash.close();
      rebuild(next, refs, (before + CHIP_TOKEN).length);
      emit(next, refs);
    },
    [rebuild, emit, slash],
  );

  const selectRef = useRef<(reference: FileReference) => void>(() => {});
  selectRef.current = handleSelect;
  const imageInsertRef = useRef<(reference: FileReference) => void>(() => {});
  imageInsertRef.current = insertReferenceAtCaret;
  const slashSelectRef = useRef<(skill: SkillEntry) => void>(() => {});
  slashSelectRef.current = handleSkillSelect;
  useImperativeHandle(ref, () => ({
    focus: () => rootRef.current?.focus({ preventScroll: true }),
    insertReference: (reference) => selectRef.current(reference),
    insertSkill: (skill) => slashSelectRef.current(skill),
    insertImage: (reference) => imageInsertRef.current(reference),
    openMention: () => {
      rootRef.current?.focus({ preventScroll: true });
      mentionRef.current = { queryStart: -1 };
      search.open();
    },
  }));

  const handleBackspace = useCallback((): boolean => {
    const root = rootRef.current;
    if (!root) return false;
    const selection = window.getSelection();
    if (!selection || !selection.isCollapsed) return false;
    const caret = caretOffset(root);
    if (caret <= 0) return false;
    const text = serializeEditor(root);
    const target = caret - 1;
    if (text[target] !== CHIP_TOKEN) return false;
    const refs = refsRef.current.slice();
    refs.splice(countTokens(text.slice(0, target)), 1);
    const next = text.slice(0, target) + text.slice(caret);
    rebuild(next, refs, target);
    emit(next, refs);
    return true;
  }, [rebuild, emit]);

  // Delete forward: when the caret sits exactly **before** a chip placeholder, delete the whole chip.
  const handleDeleteForward = useCallback((): boolean => {
    const root = rootRef.current;
    if (!root) return false;
    const selection = window.getSelection();
    if (!selection || !selection.isCollapsed) return false;
    const text = serializeEditor(root);
    const caret = caretOffset(root);
    if (caret >= text.length || text[caret] !== CHIP_TOKEN) return false;
    const refs = refsRef.current.slice();
    refs.splice(countTokens(text.slice(0, caret)), 1);
    const next = text.slice(0, caret) + text.slice(caret + 1);
    rebuild(next, refs, caret);
    emit(next, refs);
    return true;
  }, [rebuild, emit]);

  // Left/right arrow keys across chips: a contenteditable=false chip is a spot the browser
  // caret "cannot cross", so the arrow keys often stay put when the caret is next to it.
  // Here we take over only when **crossing a chip boundary**; ordinary text stays native.
  const moveCaret = useCallback((delta: -1 | 1): boolean => {
    const root = rootRef.current;
    if (!root) return false;
    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) return false;
    const text = serializeEditor(root);
    const caret = caretOffset(root);
    const next = caret + delta;
    if (next < 0 || next > text.length) return false;
    if (delta === 1 && text[caret] === CHIP_TOKEN) {
      setCaretAt(root, caret + 1);
      return true;
    }
    if (delta === -1 && text[next] === CHIP_TOKEN) {
      setCaretAt(root, next);
      return true;
    }
    return false;
  }, []);

  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (event.nativeEvent.isComposing || event.keyCode === 229) return;
      if (search.popoverOpen) {
        if (event.key === "ArrowDown") {
          event.preventDefault();
          search.moveSelection(1);
          return;
        }
        if (event.key === "ArrowUp") {
          event.preventDefault();
          search.moveSelection(-1);
          return;
        }
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          mentionRef.current = null;
          search.close();
          return;
        }
        if (event.key === "Enter" || event.key === "Tab") {
          event.preventDefault();
          const result = search.confirmSelection();
          if (result) handleSelect(result);
          return;
        }
        return;
      }
      if (slash.open) {
        if (event.key === "ArrowDown") {
          event.preventDefault();
          slash.moveSelection(1);
          return;
        }
        if (event.key === "ArrowUp") {
          event.preventDefault();
          slash.moveSelection(-1);
          return;
        }
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          slash.close();
          return;
        }
        if (event.key === "Enter" || event.key === "Tab") {
          const result = slash.confirmSelection();
          if (result) {
            event.preventDefault();
            handleSkillSelect(result);
            return;
          }
          // No candidate to confirm: swallow Tab (to avoid focus jumping away), hand Enter back to normal submit.
          if (event.key === "Tab") {
            event.preventDefault();
            return;
          }
        }
      }
      if (event.key === "ArrowLeft" && !event.shiftKey && !event.altKey && !event.metaKey && !event.ctrlKey) {
        if (moveCaret(-1)) {
          event.preventDefault();
          return;
        }
      }
      if (event.key === "ArrowRight" && !event.shiftKey && !event.altKey && !event.metaKey && !event.ctrlKey) {
        if (moveCaret(1)) {
          event.preventDefault();
          return;
        }
      }
      // Backspace / Delete: a chip is an atomic `contenteditable=false` node, and the
      // browser's default is to "select the whole chip first, then delete" — so the first
      // press only moves the caret to the chip boundary and the second actually deletes.
      // Take over directly in keydown (before the browser's default behavior): when the
      // caret is adjacent to a chip, delete manually and prevent default; otherwise return
      // false and hand back to the browser's native deletion.
      if (event.key === "Backspace" && !event.altKey && !event.metaKey && !event.ctrlKey) {
        if (handleBackspace()) {
          event.preventDefault();
          return;
        }
      }
      if (event.key === "Delete" && !event.altKey && !event.metaKey && !event.ctrlKey) {
        if (handleDeleteForward()) {
          event.preventDefault();
          return;
        }
      }
      if (shouldSendChatOnEnter(event)) {
        event.preventDefault();
        onSubmit();
      }
    },
    [search, slash, handleSelect, handleSkillSelect, handleBackspace, handleDeleteForward, onSubmit, moveCaret],
  );

  // Native `beforeinput`: React's `onBeforeInput` is an old interface synthesized from
  // textInput/keypress, where `nativeEvent.inputType` is always undefined, so the delete /
  // line-break branches never fire (a former bug). So listen to the native event directly —
  // it is the interface that can read inputType and whose preventDefault stops default editing.
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const onBeforeInput = (event: InputEvent) => {
      if (event.inputType === "insertLineBreak" || event.inputType === "insertParagraph") {
        event.preventDefault();
        insertTextAtCaret(root, "\n");
        inputRef.current();
      }
    };
    root.addEventListener("beforeinput", onBeforeInput);
    return () => root.removeEventListener("beforeinput", onBeforeInput);
  }, []);

  // Click a chip: a chip is a contenteditable=false node, and the browser will not place
  // the caret "between the chip and adjacent text". Depending on whether the click lands on
  // the chip's left/right half, manually snap the caret before/after the chip.
  const handleMouseDown = useCallback((event: React.MouseEvent<HTMLDivElement>) => {
    const root = rootRef.current;
    if (!root) return;
    const chip = closestChip(root, event.target as Node);
    if (!chip) return;
    const offset = chipOffset(root, chip);
    if (offset == null) return;
    event.preventDefault();
    const rect = chip.getBoundingClientRect();
    const after = event.clientX > rect.left + rect.width / 2;
    setCaretAt(root, after ? offset + 1 : offset);
    root.focus({ preventScroll: true });
  }, []);

  const handlePaste = useCallback(
    async (event: React.ClipboardEvent<HTMLDivElement>) => {
      // Pasted image(s): intercept before the text branch. A clipboard screenshot has no
      // useful `text/plain`; each image file becomes an `image` chip (data URL payload),
      // inserted at the caret in order. Multiple images paste as multiple chips.
      const files = Array.from(event.clipboardData?.items ?? [])
        .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
        .map((item) => item.getAsFile())
        .filter((file): file is File => file != null);
      if (files.length > 0) {
        event.preventDefault();
        for (const file of files) {
          const dataUrl = await fileToDataUrl(file);
          insertReferenceAtCaret({
            path: `image:${file.name || "pasted-image"}`,
            name: file.name || "image",
            kind: "image",
            dataUrl,
          });
        }
        return;
      }

      const data = event.clipboardData?.getData("text/plain");
      if (data == null) return;
      event.preventDefault();
      const root = rootRef.current;
      if (!root) return;
      insertTextAtCaret(root, data);
      handleInput();
    },
    [handleInput, insertReferenceAtCaret],
  );

  // Keep the native listener registered at mount always calling the latest implementation.
  inputRef.current = handleInput;

  return (
    <div
      ref={rootRef}
      id={id}
      role="textbox"
      aria-label={label}
      aria-multiline="true"
      contentEditable={!disabled}
      suppressContentEditableWarning
      spellCheck={false}
      data-mention-editor
      data-empty={empty ? "true" : undefined}
      data-placeholder={placeholder}
      className={cn(
        "max-h-40 min-h-12 overflow-y-auto whitespace-pre-wrap break-words px-2 py-2 text-base leading-relaxed text-ink outline-none",
        disabled && "cursor-not-allowed opacity-45",
        className,
      )}
      onInput={handleInput}
      onKeyDown={handleKeyDown}
      onMouseDown={handleMouseDown}
      onPaste={(event) => void handlePaste(event)}
    />
  );
});
