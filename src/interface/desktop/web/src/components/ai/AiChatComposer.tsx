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
  children,
  sendLabel = "发送消息",
  onValueChange,
  onSend,
  onStop,
  ...props
}: AiChatComposerProps) {
  const unavailable = disabled || busy;
  return (
    <form
      className={cn(
        "relative flex shrink-0 flex-col rounded-card bg-inset p-2 transition-[background-color,box-shadow] duration-150 ease-product focus-within:bg-surface focus-within:ring-2 focus-within:ring-ink/8",
        className,
      )}
      data-ai-chat-composer
      {...props}
      onSubmit={(event) => {
        event.preventDefault();
        if (unavailable || !value.trim()) return;
        onSend();
        event.currentTarget
          .querySelector<HTMLTextAreaElement>("textarea")
          ?.focus({ preventScroll: true });
      }}
    >
      {children}
      <div className="order-3 min-w-0">
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
            if (shouldSendChatOnEnter(event) && value.trim() && !unavailable) {
              event.preventDefault();
              onSend();
            }
          }}
        />
      </div>
      <div className="order-4 mt-1 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1">{leadingAction}</div>
        <div className="flex shrink-0 items-center gap-1">
          {trailingAction}
          {busy && onStop ? (
            <Button
              type="button"
              variant="secondary"
              size="icon-sm"
              aria-label="停止生成"
              onClick={(event) => {
                event.preventDefault();
                onStop();
              }}
            >
              <Square data-icon="inline-start" fill="currentColor" />
            </Button>
          ) : (
            <Button
              type="submit"
              variant="primary"
              size="icon-sm"
              aria-label={sendLabel}
              disabled={unavailable || !value.trim()}
            >
              <ArrowUp data-icon="inline-start" />
            </Button>
          )}
        </div>
      </div>
    </form>
  );
}
