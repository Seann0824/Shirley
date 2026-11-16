import { Icon } from "./Icon";

const PRESET_AVATARS = [
  "/avatars/nono-classic.svg",
  "/avatars/nono-focus.svg",
  "/avatars/nono-scout.svg",
  "/avatars/nono-maker.svg",
  "/avatars/nono-calm.svg",
  "/avatars/nono-spark.svg",
];

function hash(text: string): number {
  let value = 0;
  for (const char of text) value = (value * 31 + char.charCodeAt(0)) >>> 0;
  return value;
}

/** The face of a bot. The three platform bots have their own marks; others get a preset. */
export function BotAvatar({ name, size = 25 }: { name: string; size?: number }) {
  switch (name) {
    case "nono":
    case "default":
      return (
        <span>
          <i />
          <i />
        </span>
      );
    case "patch":
      return <Icon name="code" size={size} />;
    case "scout":
      return <Icon name="globe" size={size} />;
    default:
      return <img src={PRESET_AVATARS[hash(name) % PRESET_AVATARS.length]} alt="" />;
  }
}

/** CSS modifier for the bot's colour scheme (`nono` / `patch` / `scout` / `custom-bot`). */
export function botTone(name: string): string {
  return ["nono", "default", "patch", "scout"].includes(name)
    ? name === "default"
      ? "nono"
      : name
    : "custom-bot";
}
