import { Check, Copy } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { copyText } from "@/lib/clipboard";
import { cn } from "@/ui/utils";

/**
 * Copy-to-clipboard affordance for a single transcript message.
 *
 * Sits at the **end** of the message (after the body), on the message's own side — assistant
 * replies are left-aligned, so the action is too. It keeps its layout box at all times and
 * only fades in on hover, so appearing never shifts the text. `text` is the **raw** message content (not the
 * rendered markdown), which is what people actually want on the clipboard.
 */
export function MessageCopyButton({
  text,
  label,
  disabled,
  className,
}: {
  text: string;
  /** Accessible/tooltip title, e.g. "Copy message". */
  label: string;
  disabled?: boolean;
  /** Placement override; the message column decides which edge the action sits on. */
  className?: string;
}) {
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  const handleClick = useCallback(async () => {
    if (!text) return;
    const ok = await copyText(text);
    setCopied(ok);
    setFailed(!ok);
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      setCopied(false);
      setFailed(false);
    }, 1600);
  }, [text]);

  if (!text || disabled) return null;

  const title = failed ? "Copy failed" : copied ? "Copied" : label;

  return (
    <button
      type="button"
      aria-label={label}
      title={title}
      onClick={() => void handleClick()}
      className={cn(
        // Reserved space, not `hidden`: the action is always laid out and only its opacity
        // changes on hover, so revealing it can never reflow the message around it. Touch
        // devices have no hover, so they keep it permanently visible (same rule as
        // `index.css`'s `(hover: none)` block, which already forces it to display).
        "message-edit-action flex shrink-0 items-center self-center rounded-md p-1.5 opacity-0 pointer-events-none transition-opacity hover:bg-surface-2 group-hover:pointer-events-auto group-hover:opacity-100 group-focus-within:pointer-events-auto group-focus-within:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:pointer-events-auto [@media(hover:none)]:opacity-100",
        copied ? "text-ink" : "text-muted hover:text-ink",
        className,
      )}
      data-message-copy
    >
      {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
    </button>
  );
}
