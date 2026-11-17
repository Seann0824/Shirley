import { useState, type ReactNode } from "react";

import { useBots } from "./state/useBots";
import { useTasks } from "./state/useTasks";
import { useWide } from "./state/useWide";
import { Chat } from "./views/Chat";
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
  const [seed, setSeed] = useState<string | undefined>(undefined);
  const wide = useWide();
  const bots = useBots();
  const tasks = useTasks(wide || tab === "home" || tab === "tasks" || tab === "work");

  const selectedBot = bots.bots.find((bot) => bot.name === selected) ?? bots.bots[0];

  const openBot = (name: string) => {
    setSelected(name);
    setSeed(undefined);
    setTab("chat");
  };

  const home = (
    <Home
      bots={bots}
      tasks={tasks.tasks}
      onNavigate={setTab}
      onOpenBot={openBot}
      onDispatch={(brief) => tasks.enqueue(brief)}
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
          seed={seed}
          onBack={() => setTab("home")}
          onMemory={() => setTab("memory")}
          onTurnFinished={() => void bots.refresh()}
        />
      )}
      {view === "memory" && selectedBot && <Memory bot={selectedBot} onBack={() => setTab("chat")} />}
      {view === "new" && (
        <NewBot
          onBack={() => setTab("home")}
          onSubmit={(description) => {
            setSelected(BUILDER_BOT);
            setSeed(createBotPrompt(description));
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
