import { ArrowUp, Square } from "lucide-react";
import type { ComponentPropsWithoutRef, ReactNode, Ref } from "react";
import { Button } from "@/ui/button";
import { FIELD_SHELL } from "@/ui/field";
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
  /** Left cluster of the action row: e.g. the "+" attach button and the access pill. */
  leadingAction?: ReactNode;
  /** Actions placed to the right of the action row, before the send button (e.g. the model switcher). */
  trailingAction?: ReactNode;
  /**
   * A custom input control replacing the built-in `<textarea>` (e.g. a rich input with
   * inline `@` reference chips). When provided, textarea-specific props like `value` /
   * `textareaRef` / `textareaOnKeyDown` are the caller's responsibility.
   */
  inputSlot?: ReactNode;
  children?: ReactNode;
  sendLabel?: string;
  onValueChange: (value: string) => void;
  onSend: () => void;
  onStop?: () => void;
};

/**
 * The chat composer: a single rounded pill that mirrors the reference layout — the text
 * area sits on top, and an action row runs along the bottom with a left cluster (attach /
 * access) and a right cluster (model switcher + round send button). It is a pure layout
 * shell; all controls are supplied by the caller through `leadingAction` / `trailingAction`
 * / `inputSlot`, so it never owns business state.
 */
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
  sendLabel = "Send message",
  onValueChange,
  onSend,
  onStop,
  ...props
}: AiChatComposerProps) {
  // A single adaptive button: non-empty input = send (queued if running); empty input while generating = pause/stop.
  const hasText = value.trim().length > 0;
  const showStop = !hasText && busy && Boolean(onStop);
  return (
    <form
      className={cn(
        cn("relative flex shrink-0 flex-col gap-0.5 p-2", FIELD_SHELL),
        className,
      )}
      data-ai-chat-composer
      {...props}
      onSubmit={(event) => {
        event.preventDefault();
        // Allow submit even while busy: the caller queues it as pending and auto-sends after the previous turn ends.
        if (disabled || !value.trim()) return;
        onSend();
        // Return focus to the input control after sending (built-in textarea or the rich input in inputSlot).
        event.currentTarget
          .querySelector<HTMLElement>("textarea, [contenteditable='true'], input")
          ?.focus({ preventScroll: true });
      }}
    >
      {children}
      <div className="order-1 min-w-0 px-0.5 pt-0.5">
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
                "max-h-40 min-h-9 resize-none overflow-y-auto rounded-none border-0 bg-transparent p-0 [field-sizing:content] hover:bg-transparent focus:border-transparent focus:bg-transparent focus:ring-0",
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
      <div className="order-2 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1">{leadingAction}</div>
        <div className="flex shrink-0 items-center gap-1">
          {trailingAction}
          {showStop ? (
            <Button
              type="button"
              variant="primary"
              size="icon-xs"
              aria-label="Stop generating"
              disabled={disabled}
              className="rounded-full"
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
              size="icon-xs"
              aria-label={busy ? "Add to queue, send after the previous turn finishes" : sendLabel}
              disabled={disabled || !hasText}
              className="rounded-full"
            >
              <ArrowUp data-icon="inline-start" />
            </Button>
          )}
        </div>
      </div>
    </form>
  );
}
