import { useEffect, useState } from "react";

import { api } from "../api/bridge";
import type { Bot, PoolTask } from "../api/types";
import { BotAvatar, botTone } from "../components/BotAvatar";
import { SwipeBotRow } from "../components/BotRow";
import { ComposerDock, type Attachment } from "../components/Composer";
import { briefWithAttachments, uploadAll } from "../components/attachments";
import { Icon, type IconName } from "../components/Icon";
import type { useBots } from "../state/useBots";
import { useWide } from "../state/useWide";

type Tab = "home" | "tasks" | "work" | "memory" | "chat" | "new";

const TOOL_ICONS: Record<string, IconName> = {
  bash: "code",
  grep: "search",
  read: "file",
  media: "image",
  web_search: "globe",
};

function HomeHeader({ onNavigate, pending }: { onNavigate: (view: Tab) => void; pending: boolean }) {
  return (
    <header className="home-header">
      <span className="mini-avatar" aria-hidden="true">
        <span>
          <i />
          <i />
        </span>
        <b />
      </span>
      <strong>Shirley</strong>
      <div className="header-actions">
        <button onClick={() => onNavigate("new")} aria-label="创建 Bot">
          <Icon name="plus" size={20} />
        </button>
        <button onClick={() => onNavigate("tasks")} aria-label="任务">
          <Icon name="clock" size={20} />
          {pending && <i />}
        </button>
        <button onClick={() => onNavigate("work")} aria-label="交付">
          <Icon name="box" size={20} />
        </button>
      </div>
    </header>
  );
}

function CapabilityStrip({ bot }: { bot: Bot }) {
  switch (bot.name) {
    case "nono":
      return (
        <div className="capability-images">
          {["web", "files", "create"].map((item) => (
            <img key={item} src={`/capabilities/${item}.svg`} alt="" />
          ))}
        </div>
      );
    case "patch":
      return (
        <div className="bot-visual-strip code-strip">
          <span>&lt;/&gt;</span>
          <i />
          <i />
        </div>
      );
    case "scout":
      return (
        <div className="bot-visual-strip web-strip">
          <Icon name="globe" size={16} />
          <i />
          <Icon name="search" size={15} />
        </div>
      );
    default:
      return (
        <div className="bot-visual-strip custom-strip">
          {bot.tools.slice(0, 3).map((tool) => (
            <Icon key={tool} name={TOOL_ICONS[tool] ?? "tool"} size={14} />
          ))}
        </div>
      );
  }
}

export function Home({
  bots,
  tasks,
  onNavigate,
  onOpenBot,
  onDispatch,
  selected,
}: {
  bots: ReturnType<typeof useBots>;
  tasks: PoolTask[];
  onNavigate: (view: Tab) => void;
  onOpenBot: (bot: string) => void;
  /** Queue a task; the platform picks the bot. Resolves to the bot it chose. */
  onDispatch: (brief: string) => Promise<string | null>;
  /** The bot whose conversation is open beside the list (two-pane layout only). */
  selected?: string;
}) {
  const visible = bots.visible;
  // On a wide screen the conversation beside the list takes page-level pastes.
  const wide = useWide();
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [dispatched, setDispatched] = useState<{ bot: string; nonce: number } | null>(null);
  const dispatchedBot = visible.find((bot) => bot.name === dispatched?.bot);

  useEffect(() => {
    if (!dispatched) return;
    const timer = window.setTimeout(() => setDispatched(null), 2600);
    return () => window.clearTimeout(timer);
  }, [dispatched]);

  const busyBots = new Set(
    tasks.filter((task) => task.status === "in_progress" || task.status === "to_do").map((task) => task.bot),
  );
  const pendingReview = tasks.some((task) => task.status === "in_review" || task.status === "blocked");

  const dispatch = async (text: string, attachments: Attachment[]) => {
    // The media library is shared by every bot, so any runtime can take the upload.
    const uploader = visible[0]?.name;
    if (!uploader) return;
    const { entries, failed } = await uploadAll(attachments, (file) => api.upload(uploader, file));
    setUploadError(failed.length ? `附件上传失败：${failed.join("、")}` : null);
    const brief = text || (entries.length ? "处理这些附件" : "");
    if (!brief) return;
    const bot = await onDispatch(briefWithAttachments(brief, entries));
    if (bot) setDispatched({ bot, nonce: Date.now() });
  };

  return (
    <>
      <HomeHeader onNavigate={onNavigate} pending={pendingReview} />

      {bots.error && <p className="error-note">无法连接服务器：{bots.error}</p>}
      {uploadError && <p className="error-note">{uploadError}</p>}

      {dispatched && dispatchedBot && (
        <section className="route-result" role="status" aria-label={`任务已交给 ${dispatchedBot.display_name}`}>
          <div className="route-symbol">
            <span>
              <Icon name="chat" size={14} />
            </span>
          </div>
          <div className="dispatch-track">
            <i />
          </div>
          <div className="dispatch-destination">
            <div className={`assigned-avatar ${botTone(dispatched.bot)}`}>
              <BotAvatar name={dispatched.bot} size={18} />
            </div>
            <strong>{dispatchedBot.display_name}</strong>
          </div>
        </section>
      )}

      <section className="home-bot-list">
        {visible.map((bot) => {
          const tone = botTone(bot.name);
          const receiving = dispatched?.bot === bot.name;
          return (
            <SwipeBotRow
              key={bot.name}
              name={bot.display_name}
              className={`${tone} ${receiving ? "receiving" : ""} ${selected === bot.name ? "selected" : ""}`}
              pinned={bots.prefs.pinned.includes(bot.name)}
              muted={bots.prefs.muted.includes(bot.name)}
              onOpen={() => onOpenBot(bot.name)}
              onTogglePin={() => bots.togglePin(bot.name)}
              onToggleMute={() => bots.toggleMute(bot.name)}
              onDelete={() => bots.hide(bot.name)}
            >
              <div className={`home-bot-avatar ${tone === "custom-bot" ? "custom-avatar" : ""}`}>
                <BotAvatar name={bot.name} />
                <b />
              </div>
              <div className="home-bot-info">
                <div>
                  <strong>{bot.display_name}</strong>
                  <span className={`status ${busyBots.has(bot.name) ? "busy" : ""}`}>
                    <i /> {busyBots.has(bot.name) ? "工作中" : "空闲"}
                  </span>
                </div>
                <CapabilityStrip bot={bot} />
              </div>
            </SwipeBotRow>
          );
        })}
        {visible.length === 0 && !bots.error && <p className="empty-note">没有可用的 Bot</p>}
      </section>

      <ComposerDock
        claimsPagePaste={!wide}
        onSend={(text, attachments) => void dispatch(text, attachments)}
        placeholder="交代一件事…"
      />
    </>
  );
}
