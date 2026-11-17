import { Fragment, useEffect, useLayoutEffect, useRef, useState } from "react";

import { api } from "../api/bridge";
import type { Bot, RenderPart, ToolCallWire } from "../api/types";
import { BotAvatar, botTone } from "../components/BotAvatar";
import { ComposerDock, type Attachment } from "../components/Composer";
import { uploadAll, userMessageParts } from "../components/attachments";
import { Icon } from "../components/Icon";
import { MarkdownBlock, splitBlocks } from "../components/Markdown";
import { showsToolActivity, toolActivity } from "../components/toolActivity";
import { TaskCard } from "../components/TaskCard";
import { cardsByTurnEnd } from "../state/conversation";
import { useConversation, type ConversationItem } from "../state/useConversation";
import type { useTasks } from "../state/useTasks";

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

/** A field of a tool call's JSON output or arguments, if it parses. */
function field(json: string | null, key: string): string | null {
  if (!json) return null;
  try {
    const value = (JSON.parse(json) as Record<string, unknown>)[key];
    return typeof value === "string" ? value : null;
  } catch {
    return null;
  }
}

type Tasks = ReturnType<typeof useTasks>;

/** Where the bot handed work off: the task's card, kept current, in place. */
function DispatchedTask({ bot, call, tasks }: { bot: Bot; call: ToolCallWire; tasks: Tasks }) {
  const id = field(call.output, "task_id");
  const task = id ? tasks.tasks.find((entry) => entry.id === id) : undefined;
  const title = field(call.arguments, "title") ?? "任务";
  let body;
  if (task) {
    body = <TaskCard task={task} actions={tasks} />;
  } else if (call.ok === false) {
    body = <p className="task-line">没能派出「{title}」</p>;
  } else if (!id) {
    body = <p className="task-line">正在派出「{title}」…</p>;
  } else if (!tasks.loaded) {
    return null;
  } else {
    body = <p className="task-line">「{title}」已从任务列表移除</p>;
  }
  return (
    <div className="message-row bot-message task-row">
      <div className={`message-avatar ${botTone(bot.name)}`}>
        <BotAvatar name={bot.name} size={27} />
      </div>
      {body}
    </div>
  );
}

/** A picture stands on its own, like a photo in a chat app; a file or link gets a chip. */
function mediaClass(part: RenderPart): string {
  return part.kind === "image" ? "message-media" : `message-bubble media-bubble ${part.kind}`;
}

const TASK_TOOLS = new Set(["dispatch_task", "task_note"]);

/** The user's latest message can be taken back: tap it, then 撤回. */
interface Recall {
  open: boolean;
  onToggle: () => void;
  onRecall: () => void;
}

function Item({ bot, item, tasks, recall }: { bot: Bot; item: ConversationItem; tasks: Tasks; recall?: Recall }) {
  switch (item.kind) {
    case "message":
      if (item.role === "user") {
        // Every picture or file is its own message; the text is another.
        const media = item.parts.filter((part) => part.kind !== "text");
        const tap = recall ? { onClick: recall.onToggle, role: "button", tabIndex: 0 } : {};
        return (
          <>
            {media.map((part, index) => (
              <div className="message-row user-message" key={`m${index}`}>
                <div className={`${mediaClass(part)} ${recall ? "recallable" : ""}`} {...tap}>
                  <Parts bot={bot.name} parts={[part]} />
                </div>
              </div>
            ))}
            {item.content && (
              <div className="message-row user-message">
                <div className={`message-bubble ${recall ? "recallable" : ""}`} {...tap}>
                  <p>{item.content}</p>
                </div>
              </div>
            )}
            {recall?.open && (
              <div className="recall-row">
                <button onClick={recall.onRecall}>撤回并编辑</button>
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
                <div className={mediaClass(part)}>
                  <Parts bot={bot.name} parts={[part]} />
                </div>
              </div>
            ))}
        </>
      );
    case "tools": {
      const other = item.calls.filter((call) => !TASK_TOOLS.has(call.name));
      // A call restored from the log carries no status; its JSON output is the proof it worked.
      const noted = item.calls.filter(
        (call) => call.name === "task_note" && call.ok !== false && field(call.output, "task_id"),
      );
      return (
        <>
          {showsToolActivity(toolActivity(other, item.settled === true)) && (
            <div className="message-row bot-message tools-row">
              <div className={`message-avatar ${botTone(bot.name)}`}>
                <BotAvatar name={bot.name} size={27} />
              </div>
              <ToolActivity bot={bot.name} calls={other} settled={item.settled === true} />
            </div>
          )}
          {noted.map((call) => (
            <div className="system-line" key={call.id}>
              已补充到「{field(call.output, "title") ?? "任务"}」
            </div>
          ))}
        </>
      );
    }
    case "compact":
      return <div className="system-line">较早的对话已整理成摘要</div>;
  }
}

/** A message to send as soon as the conversation opens (home screen, "new bot"). */
export interface Seed {
  text: string;
  attachments: Attachment[];
  nonce: number;
}

export function Chat({
  bot,
  seed,
  tasks,
  onBack,
  onSkills,
  onTurnFinished,
}: {
  bot: Bot;
  seed?: Seed;
  tasks: Tasks;
  onBack: () => void;
  onSkills: () => void;
  onTurnFinished?: () => void;
}) {
  const conversation = useConversation(bot.name, onTurnFinished);
  const scrollRef = useRef<HTMLDivElement>(null);
  const seeded = useRef<number | null>(null);

  useEffect(() => {
    if (seed && conversation.loaded && seeded.current !== seed.nonce) {
      seeded.current = seed.nonce;
      void send(seed.text, seed.attachments);
    }
    // `send` is recreated every render; the nonce is what makes a seed fire once.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seed, conversation.loaded]);

  // Results waiting for the user's verdict, possibly scrolled out of sight.
  const waiting = tasks.tasks.filter((task) => task.bot === bot.name && task.status === "in_review");
  const showWaiting = () => {
    const card = waiting
      .map((task) => document.getElementById(`task-${task.id}`))
      .find((element) => element !== null);
    card?.scrollIntoView({ behavior: "smooth", block: "center" });
  };

  // The conversation opens at its latest message and stays pinned there while it grows (new
  // tokens, images and cards loading), unless the user has scrolled up to read.
  const pinned = useRef(true);
  const toBottom = () => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  };
  useLayoutEffect(() => {
    if (pinned.current) toBottom();
  }, [conversation.items, conversation.loaded]);
  useEffect(() => {
    const el = scrollRef.current;
    const content = el?.firstElementChild;
    if (!el || !content) return;
    const onScroll = () => {
      pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    };
    const observer = new ResizeObserver(() => {
      if (pinned.current) toBottom();
    });
    el.addEventListener("scroll", onScroll, { passive: true });
    observer.observe(content);
    observer.observe(el);
    return () => {
      el.removeEventListener("scroll", onScroll);
      observer.disconnect();
    };
  }, []);

  const send = async (text: string, attachments: Attachment[]) => {
    pinned.current = true;
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
  const cards = cardsByTurnEnd(items);
  // Only the latest message can be taken back, like a chat app's 撤回.
  let lastUser = -1;
  items.forEach((item, index) => {
    if (item.kind === "message" && item.role === "user") lastUser = index;
  });
  const [recallOpen, setRecallOpen] = useState(false);
  const [draft, setDraft] = useState<{ text: string; nonce: number } | undefined>(undefined);
  useEffect(() => setRecallOpen(false), [lastUser]);
  const takeBack = async () => {
    setRecallOpen(false);
    try {
      const text = await conversation.recall();
      setDraft({ text, nonce: Date.now() });
    } catch (reason) {
      conversation.report(reason instanceof Error ? reason.message : String(reason));
    }
  };
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
        <button className="skills-button" onClick={onSkills} aria-label={`${bot.display_name} 的技能`}>
          <Icon name="spark" size={19} />
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
              <Fragment key={index}>
              <Item
                bot={bot}
                item={item}
                tasks={tasks}
                recall={
                  index === lastUser
                    ? {
                        open: recallOpen,
                        onToggle: () => setRecallOpen((open) => !open),
                        onRecall: () => void takeBack(),
                      }
                    : undefined
                }
              />
              {cards.get(index)?.map((call) => (
                <DispatchedTask key={call.id} bot={bot} call={call} tasks={tasks} />
              ))}
              </Fragment>
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
        above={
          waiting.length > 0 ? (
            <button className="results-pill" onClick={showWaiting}>
              <Icon name="check" size={13} />
              {waiting.length === 1 ? `「${waiting[0].title}」做好了` : `${waiting.length} 个结果等你看`}
            </button>
          ) : undefined
        }
        onSend={(text, attachments) => void send(text, attachments)}
        draft={draft}
        busy={running}
        onStop={() => void conversation.cancel()}
        claimsPagePaste
      />
    </section>
  );
}
