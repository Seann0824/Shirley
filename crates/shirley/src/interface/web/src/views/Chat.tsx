import { useEffect, useRef, useState } from "react";

import { api } from "../api/bridge";
import type { Bot, RenderPart, ToolCallWire } from "../api/types";
import { BotAvatar, botTone } from "../components/BotAvatar";
import { ComposerDock, readAsDataUrl, type Attachment } from "../components/Composer";
import { Icon } from "../components/Icon";
import { useConversation, type ConversationItem } from "../state/useConversation";

const TOOL_LABELS: Record<string, string> = {
  bash: "运行命令",
  grep: "搜索内容",
  read: "读取文件",
  media: "查看附件",
  web_search: "搜索网页",
  todo: "整理步骤",
  recall: "回忆",
  activate_skill: "启用技能",
  read_skill_file: "读取技能",
  subagent: "派出子任务",
  create_bot: "创建 Bot",
};

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

function ToolLines({ bot, calls }: { bot: string; calls: ToolCallWire[] }) {
  return (
    <div className="tool-lines">
      {calls.map((call) => (
        <div className={`tool-line ${call.ok === null ? "running" : call.ok ? "ok" : "failed"}`} key={call.id}>
          <span className="tool-state">
            {call.ok === null ? <i /> : <Icon name={call.ok ? "check" : "stop"} size={12} />}
          </span>
          <span>{TOOL_LABELS[call.name] ?? call.name}</span>
          {call.parts.length > 0 && (
            <div className="tool-parts">
              <Parts bot={bot} parts={call.parts.filter((part) => part.kind !== "text")} />
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

function Item({ bot, item }: { bot: Bot; item: ConversationItem }) {
  switch (item.kind) {
    case "message":
      if (item.role === "user") {
        return (
          <div className="message-row user-message">
            <div className="message-bubble multimodal-message">
              {item.parts.some((part) => part.kind === "image") && (
                <div className="sent-attachments">
                  <Parts bot={bot.name} parts={item.parts.filter((part) => part.kind === "image")} />
                </div>
              )}
              {item.content && <p>{item.content}</p>}
            </div>
          </div>
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
      return (
        <div className="message-row bot-message">
          <div className={`message-avatar ${botTone(bot.name)}`}>
            <BotAvatar name={bot.name} size={27} />
          </div>
          {item.streaming && !item.content ? (
            <div className="message-bubble typing-bubble" aria-label={`${bot.display_name} 正在输入`}>
              <i />
              <i />
              <i />
            </div>
          ) : (
            <div className={`message-bubble ${item.streaming ? "streaming" : ""}`}>
              <p>{item.content}</p>
              <Parts bot={bot.name} parts={item.parts.filter((part) => part.kind !== "text")} />
            </div>
          )}
        </div>
      );
    case "tools":
      return (
        <div className="message-row bot-message tools-row">
          <div className={`message-avatar ${botTone(bot.name)}`}>
            <BotAvatar name={bot.name} size={27} />
          </div>
          <ToolLines bot={bot.name} calls={item.calls} />
        </div>
      );
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
  const [listening, setListening] = useState<{ on: boolean; transcript: string }>({ on: false, transcript: "" });
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
    const refs: string[] = [];
    for (const attachment of attachments) {
      try {
        refs.push(
          attachment.kind === "image"
            ? await readAsDataUrl(attachment.file)
            : await api.upload(bot.name, attachment.file),
        );
      } catch {
        // A failed attachment is reported by the server-side turn; the text still goes.
      }
    }
    await conversation.send(text || (refs.length ? "请处理这些附件" : ""), refs);
  };

  const { items, running } = conversation;
  const last = items[items.length - 1];
  const waitingForFirstToken = running && !(last?.kind === "message" && last.streaming) && last?.kind !== "tools";

  return (
    <section className={`chat-view ${listening.on ? "is-listening" : ""}`}>
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
        {listening.on ? (
          <div className="listening-state">
            <div className="voice-wave">
              {[16, 29, 45, 26, 54, 38, 62, 34, 48, 22, 39, 17].map((height, index) => (
                <i key={index} style={{ height }} />
              ))}
            </div>
            <strong>{listening.transcript || "松开发送"}</strong>
          </div>
        ) : (
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
        )}
      </div>

      <ComposerDock
        onSend={(text, attachments) => void send(text, attachments)}
        onListeningChange={(on, transcript) => setListening({ on, transcript })}
        busy={running}
        onStop={() => void conversation.cancel()}
      />
    </section>
  );
}
