import { useState } from "react";

import { api } from "../api/bridge";
import { BackHeader } from "../components/Chrome";
import { ComposerDock } from "../components/Composer";
import { Icon } from "../components/Icon";

/**
 * Describe a bot and the platform makes it: name, persona, and the market skills it needs.
 * No conversation with another bot in between; the new bot's own conversation opens when it is
 * ready.
 */
export function NewBot({
  onBack,
  onCreated,
}: {
  onBack: () => void;
  onCreated: (bot: string) => Promise<void>;
}) {
  const [brief, setBrief] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const create = async (text: string) => {
    if (!text.trim() || brief !== null) return;
    setBrief(text.trim());
    setError(null);
    try {
      const made = await api.designBot(text.trim());
      await onCreated(made.bot);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setBrief(null);
    }
  };

  return (
    <>
      <BackHeader title="新 Bot" onBack={onBack} />
      <section className="bot-builder-chat">
        <div className="builder-messages">
          <div className="builder-message ai">
            <span className="builder-avatar">
              <Icon name="bot" size={17} />
            </span>
            <p>告诉我你需要一个怎样的 Bot。我会替你定好它的名字、性格，并装上它需要的技能。</p>
          </div>
          {brief !== null && (
            <>
              <div className="builder-message user">
                <p>{brief}</p>
              </div>
              <p className="routing-note" role="status">
                <i />
                正在创建…
              </p>
            </>
          )}
          {error && <p className="error-note">{error}</p>}
        </div>
      </section>
      {/* The same composer as home and chat: talk or type the description. */}
      <ComposerDock claimsPagePaste placeholder="例如：帮我跟踪竞品并整理周报" onSend={(text) => void create(text)} />
    </>
  );
}
