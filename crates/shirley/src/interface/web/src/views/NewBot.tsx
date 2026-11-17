import { BackHeader } from "../components/Chrome";
import { ComposerDock } from "../components/Composer";
import { Icon } from "../components/Icon";

/** Turn the user's description into the request a bot-building bot acts on. */
export function createBotPrompt(description: string): string {
  return [
    "请帮我创建一个新的 Bot。我的需求：",
    description.trim(),
    "",
    "用 create_bot 工具创建它：想一个简短好记的英文 name（小写）、一个中文或英文的 display_name、",
    "一段第二人称的 persona（它是谁、负责什么、怎么工作），并只给它需要的 tools。",
    "创建完成后用一句话告诉我它的名字和擅长什么。",
  ].join("\n");
}

export function NewBot({
  onBack,
  onSubmit,
}: {
  onBack: () => void;
  onSubmit: (description: string) => void;
}) {
  return (
    <>
      <BackHeader title="新 Bot" onBack={onBack} />
      <section className="bot-builder-chat">
        <div className="builder-messages">
          <div className="builder-message ai">
            <span className="builder-avatar">
              <Icon name="bot" size={17} />
            </span>
            <p>告诉我你需要一个怎样的 Bot。我会替你定好它的名字、性格和能用的工具。</p>
          </div>
        </div>
      </section>
      {/* The same composer as home and chat: talk or type the description. */}
      <ComposerDock
        claimsPagePaste
        placeholder="例如：帮我跟踪竞品并整理周报"
        onSend={(text) => {
          if (text.trim()) onSubmit(text.trim());
        }}
      />
    </>
  );
}
