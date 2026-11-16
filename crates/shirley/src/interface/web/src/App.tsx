import { useState } from "react";

import { StatusBar } from "./components/Chrome";
import { useBots } from "./state/useBots";
import { useTasks } from "./state/useTasks";
import { Chat } from "./views/Chat";
import { Home } from "./views/Home";
import { Memory } from "./views/Memory";
import { NewBot, createBotPrompt } from "./views/NewBot";
import { Tasks } from "./views/Tasks";
import { Work } from "./views/Work";

type Tab = "home" | "tasks" | "work" | "memory" | "chat" | "new";

/** The bot that builds other bots: the generalist. */
const BUILDER_BOT = "nono";

export default function App() {
  const [tab, setTab] = useState<Tab>("home");
  const [selected, setSelected] = useState<string>(BUILDER_BOT);
  const [seed, setSeed] = useState<string | undefined>(undefined);
  const bots = useBots();
  const tasks = useTasks(tab === "home" || tab === "tasks" || tab === "work");

  const selectedBot = bots.bots.find((bot) => bot.name === selected) ?? bots.bots[0];

  const openBot = (name: string) => {
    setSelected(name);
    setSeed(undefined);
    setTab("chat");
  };

  return (
    <main className="app-canvas">
      <div className="ambient a1" />
      <div className="ambient a2" />
      <div className="phone-shell">
        <StatusBar />
        <div className={`page ${tab === "home" ? "home-page" : ""}`}>
          <div className="view-transition" key={tab}>
            {tab === "home" && (
              <Home
                bots={bots}
                tasks={tasks.tasks}
                onNavigate={setTab}
                onOpenBot={openBot}
                onDispatch={(bot, brief) => tasks.enqueue(bot, brief)}
              />
            )}
            {tab === "tasks" && <Tasks tasks={tasks} bots={bots.bots} onBack={() => setTab("home")} />}
            {tab === "work" && (
              <Work tasks={tasks.tasks} loaded={tasks.loaded} bots={bots.bots} onBack={() => setTab("home")} />
            )}
            {tab === "chat" && selectedBot && (
              <Chat
                bot={selectedBot}
                seed={seed}
                onBack={() => setTab("home")}
                onMemory={() => setTab("memory")}
                onTurnFinished={() => void bots.refresh()}
              />
            )}
            {tab === "memory" && selectedBot && <Memory bot={selectedBot} onBack={() => setTab("chat")} />}
            {tab === "new" && (
              <NewBot
                onBack={() => setTab("home")}
                onSubmit={(description) => {
                  setSelected(BUILDER_BOT);
                  setSeed(createBotPrompt(description));
                  setTab("chat");
                }}
              />
            )}
          </div>
        </div>
        <div className="home-indicator" />
      </div>
    </main>
  );
}
