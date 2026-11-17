import { useEffect, useState } from "react";

import type { Bot, PoolTask } from "../api/types";
import { BotAvatar, botTone } from "../components/BotAvatar";
import { SwipeBotRow } from "../components/BotRow";
import { ComposerDock, type Attachment } from "../components/Composer";
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
  onAsk,
  selected,
}: {
  bots: ReturnType<typeof useBots>;
  tasks: PoolTask[];
  onNavigate: (view: Tab) => void;
  onOpenBot: (bot: string) => void;
  /** Say something; the platform picks the bot and opens its conversation. */
  onAsk: (text: string, attachments: Attachment[]) => Promise<void>;
  /** The bot whose conversation is open beside the list (two-pane layout only). */
  selected?: string;
}) {
  const visible = bots.visible;
  // On a wide screen the conversation beside the list takes page-level pastes.
  const wide = useWide();
  const [asking, setAsking] = useState(false);

  const busyBots = new Set(
    tasks.filter((task) => task.status === "in_progress" || task.status === "to_do").map((task) => task.bot),
  );
  const pendingReview = tasks.some((task) => task.status === "in_review" || task.status === "blocked");

  const ask = async (text: string, attachments: Attachment[]) => {
    if (!text.trim() && !attachments.length) return;
    setAsking(true);
    try {
      await onAsk(text, attachments);
    } finally {
      setAsking(false);
    }
  };

  return (
    <>
      <HomeHeader onNavigate={onNavigate} pending={pendingReview} />

      {bots.error && <p className="error-note">无法连接服务器：{bots.error}</p>}
      {asking && (
        <p className="routing-note" role="status">
          <i />
          正在找合适的 bot…
        </p>
      )}

      <section className="home-bot-list">
        {visible.map((bot) => {
          const tone = botTone(bot.name);
          return (
            <SwipeBotRow
              key={bot.name}
              name={bot.display_name}
              className={`${tone} ${selected === bot.name ? "selected" : ""}`}
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
        onSend={(text, attachments) => void ask(text, attachments)}
        placeholder="想做点什么？"
      />
    </>
  );
}
