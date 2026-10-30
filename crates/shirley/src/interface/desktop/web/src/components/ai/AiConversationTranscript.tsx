import type { Key, ReactNode, RefObject } from "react";
import {
  Bubble,
  Message,
  MessageScroller,
  MessageScrollerContent,
  useMessageAutoScroll,
} from "@/ui/chat";
import { Pencil } from "lucide-react";
import { AiAssistantAvatar } from "./AiAssistantAvatar";
import { MessageCopyButton } from "./MessageCopyButton";
import { AiMarkdown } from "@/components/ai/AiMarkdown";
import type { FileReference } from "@/lib/file-mentions/types";
import { stripTokens } from "@/lib/file-mentions/editor-dom";
import type { RenderPartWire, StopReason } from "@/types/wire";

export type AiTranscriptMessage = {
  id: Key;
  role: "user" | "assistant";
  content: string;
  status?: "streaming" | "complete" | "error";
  /** Workspace files/dirs referenced on the user message (for `@` reference echo). */
  references?: FileReference[];
  /**
   * Rich parts of this message (`docs/render-parts.md`). A user message carries its pasted
   * images here; the transcript renders attachments separately above the text bubble.
   */
  parts?: RenderPartWire[];
  /** Processing time for this turn's assistant reply (ms); used for the "Took Ns" disclosure label. */
  durationMs?: number;
  stopReason?: StopReason;
};

export function AiConversationTranscript({
  messages,
  conversationKey,
  extraContentKey = "",
  pendingLabel,
  viewportRef: externalViewportRef,
  emptyContent,
  renderMessageContent,
  renderMessageAttachments,
  renderAfterMessage,
  editableMessageId,
  onEditMessage,
  editingMessageId,
  renderEditContent,
  contentClassName,
}: {
  messages: AiTranscriptMessage[];
  conversationKey: string;
  extraContentKey?: string;
  pendingLabel?: string;
  /** External scroll viewport ref (passed in when the parent wants sibling components like the minimap on the same viewport). */
  viewportRef?: RefObject<HTMLDivElement>;
  emptyContent?: ReactNode;
  renderMessageContent?: (message: AiTranscriptMessage, index: number) => ReactNode;
  renderMessageAttachments?: (message: AiTranscriptMessage) => ReactNode;
  renderAfterMessage?: (message: AiTranscriptMessage, index: number) => ReactNode;
  /** Id of the one message that can be edited (the last user message); others show no button. */
  editableMessageId?: Key | null;
  /** Enter edit mode for that message. */
  onEditMessage?: (message: AiTranscriptMessage) => void;
  /**
   * Id of the message currently being edited **in place**. While set, that message's
   * bubble is replaced by `renderEditContent` (the composer) instead of showing text,
   * so the user edits the original message where it sits (matching the TUI's `/rewind`).
   */
  editingMessageId?: Key | null;
  /** Inline editor rendered in place of the message whose id equals `editingMessageId`. */
  renderEditContent?: (message: AiTranscriptMessage, index: number) => ReactNode;
  /** Extra classes for the scrolled content column (e.g. centering / max-width so the
   *  viewport can span the full layout width while the reading column stays narrow). */
  contentClassName?: string;
}) {
  const contentKey = messages
    .map(
      (message) =>
        `${String(message.id)}:${message.status || "complete"}:${message.content.length}`,
    )
    .concat(extraContentKey, pendingLabel || "")
    .join("|");
  const { viewportRef } = useMessageAutoScroll({
    forceKey: conversationKey,
    contentKey,
    viewportRef: externalViewportRef,
  });

  return (
    <div className="relative min-h-0 flex-1">
      <MessageScroller
        className="h-full min-h-0"
        viewportClassName="pb-4"
        viewportRef={viewportRef}
      >
        <MessageScrollerContent className={`px-1 py-6 md:px-3 ${contentClassName ?? ""}`}>
          {messages.length === 0 && !pendingLabel && emptyContent}
          {messages.map((message, index) => {
            const isAssistant = message.role === "assistant";
            const canEdit =
              !isAssistant &&
              editableMessageId != null &&
              String(message.id) === String(editableMessageId);
            const editing =
              editingMessageId != null && String(message.id) === String(editingMessageId);
            return (
              <div
                key={message.id}
                className="group flex min-w-0 flex-col gap-3"
                data-message-id={String(message.id)}
                data-message-role={message.role}
              >
                <Message
                  align={isAssistant ? "start" : "end"}
                  className={isAssistant ? "flex-col items-start gap-2" : "flex-col items-end gap-2"}
                >
                  {isAssistant && <AiAssistantAvatar responding={message.status === "streaming"} />}
                  {!isAssistant && !editing && renderMessageAttachments?.(message)}
                  {editing && renderEditContent ? (
                    renderEditContent(message, index)
                  ) : (
                    <div className={isAssistant ? "w-full min-w-0" : "flex max-w-full items-center justify-end gap-1"}>
                      {!isAssistant && canEdit ? (
                        <button
                          type="button"
                          aria-label="Edit and resend this message"
                          title="Edit and resend"
                          onClick={() => onEditMessage?.(message)}
                          className="message-edit-action flex shrink-0 items-center rounded-md p-1.5 text-muted opacity-0 pointer-events-none transition-opacity hover:bg-surface-2 hover:text-ink group-hover:pointer-events-auto group-hover:opacity-100 group-focus-within:pointer-events-auto group-focus-within:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:pointer-events-auto [@media(hover:none)]:opacity-100"
                        >
                          <Pencil className="size-3.5" />
                        </button>
                      ) : null}
                      {isAssistant || stripTokens(message.content).trim() || message.references?.some((reference) => reference.kind === "file" || reference.kind === "dir" || reference.kind === "skill") ? (
                        <Bubble
                          variant={isAssistant ? "ghost" : "secondary"}
                          className={isAssistant ? undefined : "whitespace-pre-wrap [overflow-wrap:anywhere]"}
                        >
                          {renderMessageContent ? renderMessageContent(message, index) : message.content ? (
                            isAssistant ? <AiMarkdown content={message.content} streaming={message.status === "streaming"} /> : message.content
                          ) : message.status === "streaming" ? <span className="shimmer">Working...</span> : null}
                        </Bubble>
                      ) : null}
                    </div>
                  )}
                  {/* Copy sits at the message's end and on the assistant's own side: the
                      reading finishes at the bottom-left of a left-aligned reply. Its box is
                      always reserved, so fading it in on hover cannot move the text. */}
                  {isAssistant ? (
                    <MessageCopyButton
                      text={message.content}
                      label="Copy message"
                      disabled={message.status === "streaming" || editing}
                      className="self-start"
                    />
                  ) : null}
                </Message>
                {renderAfterMessage?.(message, index)}
              </div>
            );
          })}
          {pendingLabel && (
            <Message align="start" className="flex-col items-start gap-2" aria-live="polite">
              <AiAssistantAvatar responding />
              <Bubble variant="ghost">
                <span className="shimmer">{pendingLabel}…</span>
              </Bubble>
            </Message>
          )}
        </MessageScrollerContent>
      </MessageScroller>
    </div>
  );
}
