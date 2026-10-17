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
import { AiMarkdown } from "@/components/ai/AiMarkdown";
import type { FileReference } from "@/lib/file-mentions/types";
import type { StopReason } from "@/types/wire";

export type AiTranscriptMessage = {
  id: Key;
  role: "user" | "assistant";
  content: string;
  status?: "streaming" | "complete" | "error";
  /** Workspace files/dirs referenced on the user message (for `@` reference echo). */
  references?: FileReference[];
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
  renderAfterMessage,
  editableMessageId,
  onEditMessage,
  editingMessageId,
  renderEditContent,
}: {
  messages: AiTranscriptMessage[];
  conversationKey: string;
  extraContentKey?: string;
  pendingLabel?: string;
  /** External scroll viewport ref (passed in when the parent wants sibling components like the minimap on the same viewport). */
  viewportRef?: RefObject<HTMLDivElement>;
  emptyContent?: ReactNode;
  renderMessageContent?: (message: AiTranscriptMessage, index: number) => ReactNode;
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
        <MessageScrollerContent className="px-1 py-6 md:px-3">
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
                  className={isAssistant ? "flex-col items-start gap-2" : undefined}
                >
                  {isAssistant && <AiAssistantAvatar responding={message.status === "streaming"} />}
                  {!isAssistant && canEdit && !editing ? (
                    // The edit affordance sits next to the bubble, appears only on hover /
                    // focus-within, and reserves no layout space (hidden -> flex).
                    <button
                      type="button"
                      aria-label="Edit and resend this message"
                      title="Edit and resend"
                      onClick={() => onEditMessage?.(message)}
                      className="hidden shrink-0 items-center self-center rounded-md p-1.5 text-muted transition-colors group-hover:flex group-focus-within:flex hover:bg-surface-2 hover:text-ink"
                    >
                      <Pencil className="size-3.5" />
                    </button>
                  ) : null}
                  {editing && renderEditContent ? (
                    renderEditContent(message, index)
                  ) : (
                  <Bubble
                    variant={isAssistant ? "ghost" : "default"}
                    className={isAssistant ? undefined : "whitespace-pre-wrap"}
                  >
                    {renderMessageContent ? (
                      renderMessageContent(message, index)
                    ) : message.content ? (
                      isAssistant ? (
                        <AiMarkdown
                          content={message.content}
                          streaming={message.status === "streaming"}
                        />
                      ) : (
                        message.content
                      )
                    ) : message.status === "streaming" ? (
                      <span className="shimmer">Working...</span>
                    ) : null}
                  </Bubble>
                  )}
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
