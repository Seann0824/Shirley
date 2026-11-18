import { useEffect, useRef, useState, type ReactNode } from "react";

import { api, onAgentEvent } from "./api/bridge";
import { FileViewer } from "./components/FileViewer";
import type { Attachment } from "./components/Composer";
import { TokenGate } from "./components/TokenGate";

import { useAccessToken } from "./state/useAccessToken";
import { useBots } from "./state/useBots";
import { ReplyWatcher, registerServiceWorker, setBadge, show } from "./state/notify";
import { useTasks } from "./state/useTasks";
import { useWide } from "./state/useWide";
import { Chat, type Seed } from "./views/Chat";
import { Home } from "./views/Home";
import { NewBot } from "./views/NewBot";
import { Skills } from "./views/Skills";
import { Connections } from "./views/Connections";

type Tab = "home" | "chat" | "new" | "skills" | "connections";

/** The generalist: open first, and where unroutable messages go. */
const DEFAULT_BOT = "nono";

/** One scrolling column. The composer dock positions itself against the pane. */
function Pane({ className, viewKey, children }: { className: string; viewKey: string; children: ReactNode }) {
  return (
    <div className={`pane ${className}`}>
      <div className="page">
        <div className="view-transition" key={viewKey}>
          {children}
        </div>
      </div>
    </div>
  );
}

/** Panes with a floating composer leave room for it at the bottom. */
function paneClass(view: Tab): string {
  return view === "chat" ? "chat-pane" : view === "new" ? "new-pane" : "";
}

export default function App() {
  const [tab, setTab] = useState<Tab>("home");
  const [selected, setSelected] = useState<string>(DEFAULT_BOT);
  const [seed, setSeed] = useState<Seed | undefined>(undefined);
  const wide = useWide();
  const bots = useBots();
  const tasks = useTasks(true);
  // No token (or a refused one) means nothing can load: ask before showing an empty page.
  const access = useAccessToken();

  const selectedBot = bots.bots.find((bot) => bot.name === selected) ?? bots.bots[0];

  const openBot = (name: string) => {
    setSelected(name);
    setSeed(undefined);
    setTab("chat");
  };

  // Reply notifications: a bot finished answering while you were somewhere else.
  const looking = useRef({ bot: "", inChat: false });
  looking.current = { bot: selectedBot?.name ?? "", inChat: wide || tab === "chat" };
  const names = useRef(new Map<string, string>());
  names.current = new Map(bots.bots.map((bot) => [bot.name, bot.display_name]));
  const openRef = useRef(openBot);
  openRef.current = openBot;
  useEffect(() => {
    void registerServiceWorker();
    // Opened from a notification while the app was closed.
    const fromLink = new URLSearchParams(window.location.search).get("bot");
    if (fromLink) openRef.current(fromLink);

    const watcher = new ReplyWatcher();
    let unread = 0;
    const seen = (bot: string) =>
      document.visibilityState === "visible" &&
      document.hasFocus() &&
      looking.current.inChat &&
      looking.current.bot === bot;
    const stop = onAgentEvent((event) => {
      const reply = watcher.take(event, seen);
      if (!reply) return;
      void show(reply.bot, names.current.get(reply.bot) ?? reply.bot, reply.body);
      if (document.visibilityState !== "visible") setBadge((unread += 1));
    });
    const onVisible = () => {
      if (document.visibilityState === "visible") setBadge((unread = 0));
    };
    const onWorker = (event: MessageEvent<{ type?: string; bot?: string }>) => {
      if (event.data?.type === "open-bot" && event.data.bot) openRef.current(event.data.bot);
    };
    const onPage = (event: Event) => openRef.current((event as CustomEvent<string>).detail);
    document.addEventListener("visibilitychange", onVisible);
    navigator.serviceWorker?.addEventListener("message", onWorker);
    window.addEventListener("shirley:open-bot", onPage);
    return () => {
      stop();
      document.removeEventListener("visibilitychange", onVisible);
      navigator.serviceWorker?.removeEventListener("message", onWorker);
      window.removeEventListener("shirley:open-bot", onPage);
    };
  }, []);

  // The home composer: the platform picks the bot, then the message goes into its conversation.
  const ask = async (text: string, attachments: Attachment[]) => {
    const bot = await api
      .route(text.trim() || "处理这些附件")
      .then((answer) => answer.bot)
      .catch(() => DEFAULT_BOT);
    setSelected(bot);
    setSeed({ text, attachments, nonce: Date.now() });
    setTab("chat");
  };

  const home = (
    <Home
      bots={bots}
      tasks={tasks.tasks}
      onNavigate={setTab}
      onOpenBot={openBot}
      onAsk={ask}
      selected={wide ? selectedBot?.name : undefined}
      blocked={access.needed}
    />
  );

  // Beside the bot list there is always something open: "home" means the selected conversation.
  const view: Tab = wide && tab === "home" ? "chat" : tab;
  const content = (
    <>
      {view === "chat" && selectedBot && (
        <Chat
          key={selectedBot.name}
          bot={selectedBot}
          seed={seed?.text !== undefined && selectedBot.name === selected ? seed : undefined}
          tasks={tasks}
          onBack={() => setTab("home")}
          onSkills={() => setTab("skills")}
          onTurnFinished={() => void bots.refresh()}
        />
      )}
      {view === "skills" && selectedBot && <Skills bot={selectedBot} onBack={() => setTab("chat")} />}
      {view === "connections" && <Connections bots={bots.bots} onBack={() => setTab("home")} />}
      {view === "new" && (
        <NewBot
          onBack={() => setTab("home")}
          onCreated={async (bot) => {
            await bots.refresh();
            openBot(bot);
          }}
        />
      )}
    </>
  );

  return (
    <main className={`app-shell ${wide ? "is-wide" : ""}`}>
      <FileViewer />
      {access.needed && (
        <TokenGate busy={access.busy} error={access.error} onSubmit={(token) => void access.submit(token)} />
      )}
      {wide ? (
        <>
          <Pane className="pane-side home-pane" viewKey="home">
            {home}
          </Pane>
          <Pane className={`pane-main ${paneClass(view)}`} viewKey={`${view}:${selectedBot?.name}`}>
            {content}
          </Pane>
        </>
      ) : (
        <Pane
          className={tab === "home" ? "home-pane" : paneClass(tab)}
          viewKey={tab}
        >
          {tab === "home" ? home : content}
        </Pane>
      )}
    </main>
  );
}
