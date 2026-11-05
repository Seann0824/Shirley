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
  hasAttachments?: boolean;
  uploading?: boolean;
  rows?: number;
  maxLength?: number;
  /**
   * `stack` (default) is the desktop shape: text on top, `attach … model · send` underneath.
   * `row` is the phone shape and the one a chat app uses: `attach [input] model send` on a
   * single line, so the field the user touches and the button they press are next to each other.
   */
  layout?: "stack" | "row";
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
  hasAttachments = false,
  uploading = false,
  rows = 2,
  maxLength,
  layout = "stack",
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
  const hasContent = value.trim().length > 0 || hasAttachments;
  const showStop = !hasContent && busy && Boolean(onStop);
  const sendControls = showStop ? (
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
  ) : hasContent || layout !== "row" ? (
    <Button
      type="submit"
      variant="primary"
      size="icon-xs"
      aria-label={busy ? "Add to queue, send after the previous turn finishes" : sendLabel}
      disabled={disabled || uploading || !hasContent}
      className="rounded-full"
    >
      <ArrowUp data-icon="inline-start" />
    </Button>
  ) : null;
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
        if (disabled || uploading || !hasContent) return;
        onSend();
        // Return focus to the input control after sending (built-in textarea or the rich input in inputSlot).
        event.currentTarget
          .querySelector<HTMLElement>("textarea, [contenteditable='true'], input")
          ?.focus({ preventScroll: true });
      }}
    >
      {children}
      {layout === "row" ? (
        <div className="order-1 flex min-w-0 items-center gap-1.5">
          <div className="flex shrink-0 items-center gap-1">{leadingAction}</div>
          <div className="min-w-0 flex-1">
            {inputSlot ?? (
              <input
                ref={textareaRef as unknown as Ref<HTMLInputElement>}
                id={id}
                autoFocus={autoFocus}
                value={value}
                maxLength={maxLength}
                enterKeyHint="enter"
                aria-label={label}
                className="h-9 w-full rounded-full border-0 bg-canvas px-3 text-body-sm outline-none placeholder:text-muted"
                placeholder={placeholder}
                disabled={disabled}
                onChange={(event) => onValueChange(event.target.value)}
              />
            )}
          </div>
          <div className="flex shrink-0 items-center gap-1">
            {trailingAction}
            {sendControls}
          </div>
        </div>
      ) : (
        <>
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
                if (shouldSendChatOnEnter(event) && hasContent && !disabled && !uploading) {
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
          {sendControls}
        </div>
      </div>
        </>
      )}
    </form>
  );
}
