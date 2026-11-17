import { useState, type ReactNode } from "react";

import { api } from "./api/bridge";
import type { Attachment } from "./components/Composer";

import { useBots } from "./state/useBots";
import { useTasks } from "./state/useTasks";
import { useWide } from "./state/useWide";
import { Chat, type Seed } from "./views/Chat";
import { Home } from "./views/Home";
import { Memory } from "./views/Memory";
import { NewBot, createBotPrompt } from "./views/NewBot";
import { Tasks } from "./views/Tasks";
import { Work } from "./views/Work";

type Tab = "home" | "tasks" | "work" | "memory" | "chat" | "new";

/** The bot that builds other bots: the generalist. */
const BUILDER_BOT = "nono";

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

export default function App() {
  const [tab, setTab] = useState<Tab>("home");
  const [selected, setSelected] = useState<string>(BUILDER_BOT);
  const [seed, setSeed] = useState<Seed | undefined>(undefined);
  const wide = useWide();
  const bots = useBots();
  const tasks = useTasks(true);

  const selectedBot = bots.bots.find((bot) => bot.name === selected) ?? bots.bots[0];

  const openBot = (name: string) => {
    setSelected(name);
    setSeed(undefined);
    setTab("chat");
  };

  // The home composer: the platform picks the bot, then the message goes into its conversation.
  const ask = async (text: string, attachments: Attachment[]) => {
    const bot = await api
      .route(text.trim() || "处理这些附件")
      .then((answer) => answer.bot)
      .catch(() => BUILDER_BOT);
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
    />
  );

  // Beside the bot list there is always something open: "home" means the selected conversation.
  const view: Tab = wide && tab === "home" ? "chat" : tab;
  const content = (
    <>
      {view === "tasks" && <Tasks tasks={tasks} bots={bots.bots} onBack={() => setTab("home")} />}
      {view === "work" && (
        <Work tasks={tasks.tasks} loaded={tasks.loaded} bots={bots.bots} onBack={() => setTab("home")} />
      )}
      {view === "chat" && selectedBot && (
        <Chat
          key={selectedBot.name}
          bot={selectedBot}
          seed={seed?.text !== undefined && selectedBot.name === selected ? seed : undefined}
          tasks={tasks}
          onBack={() => setTab("home")}
          onMemory={() => setTab("memory")}
          onOpenTasks={() => setTab("tasks")}
          onTurnFinished={() => void bots.refresh()}
        />
      )}
      {view === "memory" && selectedBot && <Memory bot={selectedBot} onBack={() => setTab("chat")} />}
      {view === "new" && (
        <NewBot
          onBack={() => setTab("home")}
          onSubmit={(description) => {
            setSelected(BUILDER_BOT);
            setSeed({ text: createBotPrompt(description), attachments: [], nonce: Date.now() });
            setTab("chat");
          }}
        />
      )}
    </>
  );

  return (
    <main className={`app-shell ${wide ? "is-wide" : ""}`}>
      {wide ? (
        <>
          <Pane className="pane-side home-pane" viewKey="home">
            {home}
          </Pane>
          <Pane className={`pane-main ${view === "chat" ? "chat-pane" : ""}`} viewKey={`${view}:${selectedBot?.name}`}>
            {content}
          </Pane>
        </>
      ) : (
        <Pane
          className={tab === "home" ? "home-pane" : tab === "chat" ? "chat-pane" : ""}
          viewKey={tab}
        >
          {tab === "home" ? home : content}
        </Pane>
      )}
    </main>
  );
}
