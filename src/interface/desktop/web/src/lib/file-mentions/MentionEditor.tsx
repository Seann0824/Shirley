import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { cn } from "@/ui/utils";
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
import type { FileReference } from "./types";

export type MentionEditorHandle = {
  focus: () => void;
  /** 把某个文件/目录作为 chip 插入光标处（供外部「+」按钮调用）。 */
  insertReference: (reference: FileReference) => void;
};

/** 只保留正文里实际出现的占位符数量对应的引用（防御外部传入不一致）。 */
function deriveReferences(text: string, references: FileReference[]): FileReference[] {
  const count = countTokens(text);
  if (references.length === count) return references;
  return references.slice(0, count);
}

/**
 * `@` 引用富输入框（contenteditable）。引用 chip 是**内联节点**，与文字同行混排；
 * 正文序列化成「文本 + U+FFFC 占位符」的一条有序字符串，占位符顺序 ↔ references 顺序。
 *
 * 关键约束：**编辑期间不重渲染编辑区 DOM**（否则光标会丢）。React 只渲染根 `<div>`，
 * 内部内容完全由本组件命令式维护；仅在「外部重置 / 插入 / 删除 chip」时整体重建，
 * 并用 `pendingCaret` 恢复光标。普通输入靠浏览器原生编辑，`onInput` 只做序列化上报。
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
    onChange: (value: string, references: FileReference[]) => void;
    onSubmit: () => void;
  }
>(function MentionEditor(
  { id, label, value, references, placeholder, disabled, autoFocus, className, search, onChange, onSubmit },
  ref,
) {
  const rootRef = useRef<HTMLDivElement>(null);
  const refsRef = useRef<FileReference[]>(references);
  const lastCommittedRef = useRef<string | null>(null);
  // 当前正在编辑的 `@` 引用：queryStart = 文本里 `@` 的偏移（-1 表示来自「+」按钮的选文件）。
  const mentionRef = useRef<{ queryStart: number } | null>(null);
  const pendingCaretRef = useRef<number | null>(null);
  // 最新一版编辑命令（供挂载期一次性注册的原生 `beforeinput` 监听调用，避免闭包过期）。
  const backspaceRef = useRef<() => boolean>(() => false);
  const deleteForwardRef = useRef<() => boolean>(() => false);
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

  // 外部值变化（清空 / 回溯恢复）时重建。我们自己的输入已同步、直接跳过，避免重渲染丢光标。
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

    // 键盘删掉了 chip（浏览器把 contenteditable=false 节点当整体删）→ 对账引用。
    const chipCount = countTokens(text);
    if (chipCount !== refs.length) {
      if (chipCount < refs.length) {
        const paths = readChips(root).map((chip) => chip.path);
        refs = refs.filter((reference) => paths.includes(reference.path));
      } else {
        refs = refs.slice(0, chipCount);
      }
      emit(text, refs);
      syncMention(text, caretOffset(root), chipCount);
      return;
    }

    emit(text, refs);
    syncMention(text, caretOffset(root), chipCount);
  }, [emit, syncMention]);

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

  const selectRef = useRef<(reference: FileReference) => void>(() => {});
  selectRef.current = handleSelect;
  useImperativeHandle(ref, () => ({
    focus: () => rootRef.current?.focus({ preventScroll: true }),
    insertReference: (reference) => selectRef.current(reference),
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

  // 向前删除（Delete）：光标正好落在某个 chip 占位符**之前**时整块删除。
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

  // 左右方向键跨 chip：contenteditable=false 的 chip 是浏览器光标「跨不过去」的死角，
  // 光标贴着它时方向键往往原地不动。这里只在**跨越 chip 边界**时接管，普通文字仍走原生。
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
      if (event.key === "Enter" && !event.shiftKey) {
        event.preventDefault();
        onSubmit();
      }
    },
    [search, handleSelect, onSubmit, moveCaret],
  );

  // 原生 `beforeinput`：React 的 `onBeforeInput` 是 textInput/keypress 合成的旧接口，
  // `nativeEvent.inputType` 恒为 undefined，删除 / 换行分支根本不会触发（曾经的 bug）。
  // 因此直接监听原生事件——它才是能拿到 inputType、且 preventDefault 能阻止默认编辑的接口。
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const onBeforeInput = (event: InputEvent) => {
      if (event.inputType === "deleteContentBackward") {
        if (backspaceRef.current()) event.preventDefault();
        return;
      }
      if (event.inputType === "deleteContentForward") {
        if (deleteForwardRef.current()) event.preventDefault();
        return;
      }
      if (event.inputType === "insertLineBreak" || event.inputType === "insertParagraph") {
        event.preventDefault();
        insertTextAtCaret(root, "\n");
        inputRef.current();
      }
    };
    root.addEventListener("beforeinput", onBeforeInput);
    return () => root.removeEventListener("beforeinput", onBeforeInput);
  }, []);

  // 点击 chip：chip 是 contenteditable=false 节点，浏览器不会把光标放进「chip 与相邻文字
  // 之间」。按点击落在 chip 的左/右半区，手动把光标吸附到 chip 前 / 后。
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
    (event: React.ClipboardEvent<HTMLDivElement>) => {
      const data = event.clipboardData?.getData("text/plain");
      if (data == null) return;
      event.preventDefault();
      const root = rootRef.current;
      if (!root) return;
      insertTextAtCaret(root, data);
      handleInput();
    },
    [handleInput],
  );

  // 让挂载期注册的原生监听始终调用到最新实现。
  backspaceRef.current = handleBackspace;
  deleteForwardRef.current = handleDeleteForward;
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
      onPaste={handlePaste}
    />
  );
});
