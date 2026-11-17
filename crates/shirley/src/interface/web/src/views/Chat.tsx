import { useEffect, useRef, useState } from "react";

import { api } from "../api/bridge";
import type { Bot, RenderPart, ToolCallWire } from "../api/types";
import { BotAvatar, botTone } from "../components/BotAvatar";
import { ComposerDock, type Attachment } from "../components/Composer";
import { uploadAll, userMessageParts } from "../components/attachments";
import { Icon } from "../components/Icon";
import { MarkdownBlock, splitBlocks } from "../components/Markdown";
import { showsToolActivity, toolActivity } from "../components/toolActivity";
import { useConversation, type ConversationItem } from "../state/useConversation";

/** An image stored in the media library, loaded through a minted URL. */
function ArtifactImage({ bot, id, alt }: { bot: string; id: string; alt: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    api.assetUrl(bot, id).then((value) => !cancelled && setUrl(value)).catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [bot, id]);
  return url ? <img src={url} alt={alt} /> : <span className="asset-pending">{alt}</span>;
}

function Parts({ bot, parts }: { bot: string; parts: RenderPart[] }) {
  return (
    <>
      {parts.map((part, index) => {
        switch (part.kind) {
          case "image":
            return part.source.kind === "artifact" ? (
              <ArtifactImage key={index} bot={bot} id={part.source.id} alt={part.alt} />
            ) : (
              <img key={index} src={part.source.url} alt={part.alt} />
            );
          case "file":
            return (
              <div className="sent-file" key={index}>
                <Icon name="file" size={19} />
                <span>
                  <strong>{part.name}</strong>
                  {part.bytes != null && <small>{Math.ceil(part.bytes / 1024)} KB</small>}
                </span>
              </div>
            );
          case "link":
            return (
              <a key={index} href={part.url} target="_blank" rel="noreferrer">
                {part.title || part.url}
              </a>
            );
          default:
            return null;
        }
      })}
    </>
  );
}

/** Tools are background noise: a live turn shows a pulse, a finished one leaves no trace. */
function ToolActivity({ bot, calls, settled }: { bot: string; calls: ToolCallWire[]; settled: boolean }) {
  const activity = toolActivity(calls, settled);
  return (
    <div className="tool-activity">
      {activity.running && (
        <span className="tool-running" role="status" aria-label="正在运行">
          <i />
          <i />
          <i />
        </span>
      )}
      {activity.parts.length > 0 && (
        <div className="tool-parts">
          <Parts bot={bot} parts={activity.parts} />
        </div>
      )}
    </div>
  );
}

function Item({ bot, item }: { bot: Bot; item: ConversationItem }) {
  switch (item.kind) {
    case "message":
      if (item.role === "user") {
        // Every picture or file is its own message; the text is another.
        const media = item.parts.filter((part) => part.kind !== "text");
        return (
          <>
            {media.map((part, index) => (
              <div className="message-row user-message" key={`m${index}`}>
                <div className={`message-bubble media-bubble ${part.kind}`}>
                  <Parts bot={bot.name} parts={[part]} />
                </div>
              </div>
            ))}
            {item.content && (
              <div className="message-row user-message">
                <div className="message-bubble">
                  <p>{item.content}</p>
                </div>
              </div>
            )}
          </>
        );
      }
      if (item.role === "notice" || item.role === "error") {
        return (
          <div className="message-row bot-message">
            <div className={`message-bubble ${item.role}`}>
              <p>{item.content}</p>
            </div>
          </div>
        );
      }
      if (item.streaming && !item.content) {
        return (
          <div className="message-row bot-message">
            <div className={`message-avatar ${botTone(bot.name)}`}>
              <BotAvatar name={bot.name} size={27} />
            </div>
            <div className="message-bubble typing-bubble" aria-label={`${bot.display_name} 正在输入`}>
              <i />
              <i />
              <i />
            </div>
          </div>
        );
      }
      // One reply, several bubbles: each paragraph is its own message, like a person texting.
      return (
        <>
          {splitBlocks(item.content).map((block, index, blocks) => (
            <div className={`message-row bot-message ${index > 0 ? "continued" : ""}`} key={index}>
              <div className={`message-avatar ${botTone(bot.name)}`}>
                <BotAvatar name={bot.name} size={27} />
              </div>
              <div
                className={`message-bubble ${item.streaming && index === blocks.length - 1 ? "streaming" : ""}`}
              >
                <MarkdownBlock text={block} />
              </div>
            </div>
          ))}
          {item.parts
            .filter((part) => part.kind !== "text")
            .map((part, index) => (
              <div className="message-row bot-message continued" key={`m${index}`}>
                <div className={`message-avatar ${botTone(bot.name)}`}>
                  <BotAvatar name={bot.name} size={27} />
                </div>
                <div className={`message-bubble media-bubble ${part.kind}`}>
                  <Parts bot={bot.name} parts={[part]} />
                </div>
              </div>
            ))}
        </>
      );
    case "tools": {
      if (!showsToolActivity(toolActivity(item.calls, item.settled === true))) return null;
      return (
        <div className="message-row bot-message tools-row">
          <div className={`message-avatar ${botTone(bot.name)}`}>
            <BotAvatar name={bot.name} size={27} />
          </div>
          <ToolActivity bot={bot.name} calls={item.calls} settled={item.settled === true} />
        </div>
      );
    }
    case "compact":
      return (
        <div className="message-row bot-message">
          <div className="message-bubble notice">
            <p>（较早的对话已压缩）</p>
          </div>
        </div>
      );
  }
}

export function Chat({
  bot,
  seed,
  onBack,
  onMemory,
  onTurnFinished,
}: {
  bot: Bot;
  /** A message to send as soon as the conversation is open (the "new bot" flow). */
  seed?: string;
  onBack: () => void;
  onMemory: () => void;
  onTurnFinished?: () => void;
}) {
  const conversation = useConversation(bot.name, onTurnFinished);
  const scrollRef = useRef<HTMLDivElement>(null);
  const seeded = useRef(false);

  useEffect(() => {
    if (seed && conversation.loaded && !seeded.current) {
      seeded.current = true;
      void conversation.send(seed);
    }
  }, [seed, conversation.loaded, conversation]);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [conversation.items]);

  const send = async (text: string, attachments: Attachment[]) => {
    // Pictures go through the same upload as files: a data URL in the JSON body is a third larger
    // and hits the command body limit on a big screenshot.
    const { entries, failed } = await uploadAll(attachments, (file) => api.upload(bot.name, file));
    const sent = attachments.filter((attachment) => !failed.includes(attachment.name));
    if (!text.trim() && !entries.length) {
      conversation.report(`附件上传失败：${failed.join("、")}`);
      return;
    }
    await conversation.send(
      text,
      entries.map((entry) => entry.id),
      userMessageParts(sent),
    );
    if (failed.length) conversation.report(`没有发出去的附件：${failed.join("、")}`);
  };

  const { items, running } = conversation;
  const last = items[items.length - 1];
  const waitingForFirstToken = running && !(last?.kind === "message" && last.streaming) && last?.kind !== "tools";

  return (
    <section className="chat-view">
      <header className="chat-header">
        <button className="back-button" onClick={onBack} aria-label="返回">
          <Icon name="chevron" size={20} />
        </button>
        <div className={`chat-identity ${botTone(bot.name)}`}>
          <div>
            <BotAvatar name={bot.name} size={27} />
          </div>
          <span>
            <strong>{bot.display_name}</strong>
            <i />
          </span>
        </div>
        <button className="memory-button" onClick={onMemory} aria-label={`${bot.display_name} 的记忆`}>
          <Icon name="brain" size={19} />
        </button>
      </header>

      <div className="conversation" ref={scrollRef}>
        {
          <div className="message-thread">
            {conversation.error && <p className="error-note">{conversation.error}</p>}
            {conversation.loaded && items.length === 0 && (
              <div className="message-row bot-message">
                <div className={`message-avatar ${botTone(bot.name)}`}>
                  <BotAvatar name={bot.name} size={27} />
                </div>
                <div className="message-bubble">
                  <p>我在。直接告诉我需要完成什么。</p>
                </div>
              </div>
            )}
            {items.map((item, index) => (
              <Item key={index} bot={bot} item={item} />
            ))}
            {waitingForFirstToken && (
              <div className="message-row bot-message">
                <div className={`message-avatar ${botTone(bot.name)}`}>
                  <BotAvatar name={bot.name} size={27} />
                </div>
                <div className="message-bubble typing-bubble" aria-label={`${bot.display_name} 正在输入`}>
                  <i />
                  <i />
                  <i />
                </div>
              </div>
            )}
          </div>
        }
      </div>

      <ComposerDock
        onSend={(text, attachments) => void send(text, attachments)}
        busy={running}
        onStop={() => void conversation.cancel()}
        claimsPagePaste
      />
    </section>
  );
}
