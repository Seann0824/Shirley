import { Fragment, type Key, type ReactNode } from "react";
import { AiMarkdown } from "@/components/ai/AiMarkdown";

export type AiMessageTimelineItem = {
  key: Key;
  contentOffset?: number | null;
  content: ReactNode;
};

export function findTurnUserMessage<T extends { role: "user" | "assistant" }>(
  messages: readonly T[],
  messageIndex: number,
) {
  for (let index = messageIndex - 1; index >= 0; index -= 1) {
    if (messages[index].role === "user") return messages[index];
  }
  return undefined;
}

function normalizedOffset(offset: number | null | undefined, contentLength: number) {
  if (typeof offset !== "number" || !Number.isFinite(offset)) return 0;
  return Math.min(Math.max(Math.trunc(offset), 0), contentLength);
}

export function AiMessageTimeline({
  content,
  streaming,
  items,
  renderText,
}: {
  content: string;
  streaming: boolean;
  items: AiMessageTimelineItem[];
  renderText?: (content: string, streaming: boolean) => ReactNode;
}) {
  const characters = Array.from(content);
  const groups = new Map<number, AiMessageTimelineItem[]>();
  for (const item of items) {
    const offset = normalizedOffset(item.contentOffset, characters.length);
    groups.set(offset, [...(groups.get(offset) || []), item]);
  }
  const offsets = [...groups.keys()].sort((left, right) => left - right);
  const renderChunk =
    renderText ||
    ((chunk: string, chunkStreaming: boolean) => (
      <AiMarkdown content={chunk} streaming={chunkStreaming} />
    ));
  let cursor = 0;

  return (
    <div className="flex min-w-0 flex-col gap-3" data-ai-message-timeline>
      {offsets.map((offset) => {
        const chunk = characters.slice(cursor, offset).join("");
        cursor = offset;
        return (
          <Fragment key={`offset:${offset}`}>
            {chunk && renderChunk(chunk, false)}
            {groups.get(offset)?.map((item) => (
              <Fragment key={item.key}>{item.content}</Fragment>
            ))}
          </Fragment>
        );
      })}
      {characters.length > cursor && renderChunk(characters.slice(cursor).join(""), streaming)}
      {!content && items.length === 0 && streaming && (
        <span className="shimmer">正在思考并操作…</span>
      )}
    </div>
  );
}
