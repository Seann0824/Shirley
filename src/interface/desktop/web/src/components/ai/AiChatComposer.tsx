import { ArrowUp, Square } from "lucide-react";
import type { ComponentPropsWithoutRef, ReactNode, Ref } from "react";
import { Button } from "@/ui/button";
import { Textarea } from "@/ui/textarea";
import { shouldSendChatOnEnter } from "@/lib/chat-input";
import { cn } from "@/ui/utils";

type AiChatComposerProps = Omit<ComponentPropsWithoutRef<"form">, "children" | "onSubmit"> & {
  "data-tour"?: string;
  id: string;
  label: string;
  value: string;
  placeholder: string;
  busy: boolean;
  disabled?: boolean;
  rows?: number;
  maxLength?: number;
  textareaRef?: Ref<HTMLTextAreaElement>;
  textareaClassName?: string;
  textareaOnKeyDown?: (event: React.KeyboardEvent<HTMLTextAreaElement>) => boolean | void;
  leadingAction?: ReactNode;
  /** 放在发送按钮左侧、与之同一行的操作（如模型切换选择器）。 */
  trailingAction?: ReactNode;
  /**
   * 自定义输入控件，替换内置 `<textarea>`（如带内联 `@` 引用 chip 的富输入）。
   * 传入时 `value` / `textareaRef` / `textareaOnKeyDown` 等 textarea 专属 props 由调用方自理。
   */
  inputSlot?: ReactNode;
  children?: ReactNode;
  sendLabel?: string;
  onValueChange: (value: string) => void;
  onSend: () => void;
  onStop?: () => void;
};

export function AiChatComposer({
  id,
  label,
  value,
  placeholder,
  busy,
  disabled,
  rows = 2,
  maxLength,
  textareaRef,
  autoFocus,
  className,
  textareaClassName,
  textareaOnKeyDown,
  leadingAction,
  trailingAction,
  inputSlot,
  children,
  sendLabel = "发送消息",
  onValueChange,
  onSend,
  onStop,
  ...props
}: AiChatComposerProps) {
  // 单个按钮自适应：输入框有内容 = 发送（运行中则排队）；输入框为空且正在生成 = 暂停/停止。
  const hasText = value.trim().length > 0;
  const showStop = !hasText && busy && Boolean(onStop);
  return (
    <form
      className={cn(
        "relative flex shrink-0 flex-col rounded-card border border-line bg-surface p-2 shadow-card transition-[border-color,box-shadow] duration-150 ease-product focus-within:border-line-strong focus-within:ring-2 focus-within:ring-ink/8",
        className,
      )}
      data-ai-chat-composer
      {...props}
      onSubmit={(event) => {
        event.preventDefault();
        // busy 时也允许提交：调用方会把它排进 pending 队列，等上一轮结束自动发送。
        if (disabled || !value.trim()) return;
        onSend();
        // 发送后把焦点还给输入控件（内置 textarea 或 inputSlot 里的富输入）。
        event.currentTarget
          .querySelector<HTMLElement>("textarea, [contenteditable='true'], input")
          ?.focus({ preventScroll: true });
      }}
    >
      {children}
      <div className="order-3 min-w-0">
        {inputSlot ?? (
          <>
            <label className="sr-only" htmlFor={id}>
              {label}
            </label>
            <Textarea
              ref={textareaRef}
              id={id}
              autoFocus={autoFocus}
              value={value}
              rows={rows}
              maxLength={maxLength}
              enterKeyHint="enter"
              className={cn(
                "max-h-40 min-h-12 resize-none overflow-y-auto rounded-none border-0 bg-transparent p-2 [field-sizing:content] hover:bg-transparent focus:border-transparent focus:bg-transparent focus:ring-0",
                textareaClassName,
              )}
              placeholder={placeholder}
              disabled={disabled}
              onChange={(event) => onValueChange(event.target.value)}
              onKeyDown={(event) => {
                if (textareaOnKeyDown?.(event)) return;
                if (shouldSendChatOnEnter(event) && value.trim() && !disabled) {
                  event.preventDefault();
                  onSend();
                }
              }}
            />
          </>
        )}
      </div>
      <div className="order-4 mt-1 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1">{leadingAction}</div>
        <div className="flex shrink-0 items-center gap-1">
          {trailingAction}
          {showStop ? (
            <Button
              type="button"
              variant="secondary"
              size="icon-sm"
              aria-label="停止生成"
              disabled={disabled}
              onClick={(event) => {
                event.preventDefault();
                onStop?.();
              }}
            >
              <Square data-icon="inline-start" fill="currentColor" />
            </Button>
          ) : (
            <Button
              type="submit"
              variant="primary"
              size="icon-sm"
              aria-label={busy ? "加入队列，待上一轮结束后发送" : sendLabel}
              disabled={disabled || !hasText}
            >
              <ArrowUp data-icon="inline-start" />
            </Button>
          )}
        </div>
      </div>
    </form>
  );
}
