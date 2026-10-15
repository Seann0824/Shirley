import type { Key, ReactNode, RefObject } from "react";
import {
  Bubble,
  Message,
  MessageScroller,
  MessageScrollerContent,
  useMessageAutoScroll,
} from "@/ui/chat";
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
