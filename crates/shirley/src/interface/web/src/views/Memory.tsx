import { useState } from "react";

import type { Bot } from "../api/types";
import { BackHeader, LoadingRows } from "../components/Chrome";
import { Icon } from "../components/Icon";
import { useMemory } from "../state/useMemory";

const KIND_LABEL: Record<string, string> = {
  preference: "偏好",
  fact: "事实",
  event: "经历",
  procedure: "做法",
};

export function Memory({ bot, onBack }: { bot: Bot; onBack: () => void }) {
  const memory = useMemory(bot.name);
  const [scope, setScope] = useState<"shared" | "bot" | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);

  const active = memory.entries.filter((entry) => entry.status === "active");
  const shared = active.filter((entry) => entry.scope === "shared");
  const own = active.filter((entry) => entry.scope === "bot");
  const shown = scope === "shared" ? shared : scope === "bot" ? own : active;

  return (
    <>
      <BackHeader title={bot.display_name} onBack={onBack} />
      {!memory.loaded ? (
        <LoadingRows />
      ) : (
        <div className="content-ready">
          <section className="memory-visual">
            <div className="memory-core">
              <Icon name="brain" size={34} />
              <span className="node n1" />
              <span className="node n2" />
              <span className="node n3" />
              <span className="node n4" />
            </div>
            <h1>{bot.display_name} 记得</h1>
            <p>{active.length} 条记忆正在生效</p>
          </section>
          {memory.error && <p className="error-note">{memory.error}</p>}
          <section className="memory-map">
            <button
              className={`memory-block personal ${scope === "shared" ? "active" : ""}`}
              onClick={() => setScope(scope === "shared" ? null : "shared")}
            >
              <span className="memory-icon">
                <Icon name="bot" size={23} />
              </span>
              <div className="memory-lines">
                <i />
                <i />
                <i />
              </div>
              <strong>关于你</strong>
              <small>{shared.length}</small>
            </button>
            <button
              className={`memory-block project ${scope === "bot" ? "active" : ""}`}
              onClick={() => setScope(scope === "bot" ? null : "bot")}
            >
              <span className="memory-icon">
                <Icon name="folder" size={23} />
              </span>
              <div className="memory-lines">
                <i />
                <i />
                <i />
              </div>
              <strong>只有 {bot.display_name} 知道</strong>
              <small>{own.length}</small>
            </button>
          </section>
          <section className="memory-list">
            {shown.length === 0 && <p className="empty-note">还没有记忆。聊过几次之后，这里会慢慢填满。</p>}
            {shown.map((entry) => (
              <button
                className={`memory-entry ${expanded === entry.id ? "open" : ""}`}
                key={entry.id}
                onClick={() => setExpanded(expanded === entry.id ? null : entry.id)}
              >
                <span className="memory-kind">{KIND_LABEL[entry.kind] ?? entry.kind}</span>
                <strong>{entry.subject}</strong>
                <small>{entry.created_at}</small>
                {expanded === entry.id && <p>{entry.body}</p>}
              </button>
            ))}
          </section>
          <div className="privacy-pill">
            <Icon name="shield" size={16} />
            <span>记忆只保存在这台机器上</span>
            <i />
          </div>
        </div>
      )}
    </>
  );
}
