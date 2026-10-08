import type { Key, ReactNode } from "react";
import {
  Bubble,
  Message,
  MessageScroller,
  MessageScrollerContent,
  useMessageAutoScroll,
} from "@/ui/chat";
import { AiConversationMinimap } from "@/components/ai/AiConversationMinimap";
import { AiAssistantAvatar } from "./AiAssistantAvatar";
import { AiMarkdown } from "@/components/ai/AiMarkdown";

export type AiTranscriptMessage = {
  id: Key;
  role: "user" | "assistant";
  content: string;
  status?: "streaming" | "complete" | "error";
};

export function AiConversationTranscript({
  messages,
  conversationKey,
  extraContentKey = "",
  pendingLabel,
  showTurnMinimap = false,
  emptyContent,
  renderMessageContent,
  renderAfterMessage,
}: {
  messages: AiTranscriptMessage[];
  conversationKey: string;
  extraContentKey?: string;
  pendingLabel?: string;
  showTurnMinimap?: boolean;
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
  });
  const turnCount = messages.filter((message) => message.role === "user").length;
  const minimapVisible = showTurnMinimap && turnCount > 0;

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
                      <span className="shimmer">正在思考并操作…</span>
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
      {minimapVisible && <AiConversationMinimap messages={messages} viewportRef={viewportRef} />}
    </div>
  );
}
