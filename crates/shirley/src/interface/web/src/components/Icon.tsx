import type { ReactNode } from "react";

export type IconName =
  | "home"
  | "bot"
  | "box"
  | "brain"
  | "globe"
  | "folder"
  | "image"
  | "code"
  | "plus"
  | "arrow"
  | "paperclip"
  | "mic"
  | "check"
  | "clock"
  | "spark"
  | "chevron"
  | "shield"
  | "bell"
  | "file"
  | "search"
  | "keyboard"
  | "pin"
  | "mute"
  | "trash"
  | "chat"
  | "camera"
  | "stop"
  | "tool"
  | "plug";

const paths: Record<IconName, ReactNode> = {
  home: (
    <>
      <path d="m3 11 9-7.5 9 7.5" />
      <path d="M5.5 9.5v10h13v-10M9.5 19.5v-6h5v6" />
    </>
  ),
  bot: (
    <>
      <rect x="4" y="6" width="16" height="14" rx="5" />
      <path d="M12 6V3M9 13h.01M15 13h.01M8 17h8" />
    </>
  ),
  box: (
    <>
      <path d="m4 7 8-4 8 4-8 4-8-4Z" />
      <path d="m4 7 8 4 8-4v10l-8 4-8-4V7ZM12 11v10" />
    </>
  ),
  brain: (
    <>
      <path d="M9 4.2A4 4 0 0 0 4.7 8 4 4 0 0 0 4 15.7 4.5 4.5 0 0 0 9 21h1V4.5A4 4 0 0 0 9 4.2ZM15 4.2A4 4 0 0 1 19.3 8a4 4 0 0 1 .7 7.7 4.5 4.5 0 0 1-5 5.3h-1V4.5a4 4 0 0 1 1-.3Z" />
      <path d="M7 10h3M14 10h3M7 15h3M14 15h3" />
    </>
  ),
  globe: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18" />
    </>
  ),
  folder: <path d="M3 7h7l2 2h9v10H3V7Z" />,
  image: (
    <>
      <rect x="3" y="4" width="18" height="16" rx="3" />
      <circle cx="8.5" cy="9" r="1.5" />
      <path d="m4.5 17 4-4 3.2 3 2.5-2.5 5.3 5" />
    </>
  ),
  code: <path d="m8.5 7-5 5 5 5M15.5 7l5 5-5 5M13.5 4l-3 16" />,
  plus: <path d="M12 5v14M5 12h14" />,
  arrow: <path d="M5 12h14M14 7l5 5-5 5" />,
  paperclip: <path d="m9 12.5 5.8-5.8a3 3 0 1 1 4.2 4.2l-7.8 7.8a5 5 0 0 1-7.1-7.1l7.5-7.5" />,
  mic: (
    <>
      <rect x="8.5" y="3" width="7" height="12" rx="3.5" />
      <path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3M9 21h6" />
    </>
  ),
  check: <path d="m5 12 4.5 4.5L19 7" />,
  clock: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3.5 2" />
    </>
  ),
  spark: (
    <>
      <path d="M12 2.5c.5 4.7 2.9 7.1 7.5 7.5-4.6.4-7 2.8-7.5 7.5-.5-4.7-2.9-7.1-7.5-7.5 4.6-.4 7-2.8 7.5-7.5Z" />
      <path d="M19 16c.2 1.8 1.2 2.8 3 3-1.8.2-2.8 1.2-3 3-.2-1.8-1.2-2.8-3-3 1.8-.2 2.8-1.2 3-3Z" />
    </>
  ),
  chevron: <path d="m9 6 6 6-6 6" />,
  shield: (
    <>
      <path d="M12 3 5 6v5c0 4.6 2.5 8.1 7 10 4.5-1.9 7-5.4 7-10V6l-7-3Z" />
      <path d="m9 12 2 2 4-4" />
    </>
  ),
  bell: <path d="M18 9a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9ZM10 21h4" />,
  file: (
    <>
      <path d="M6 3h8l4 4v14H6V3Z" />
      <path d="M14 3v5h4M9 13h6M9 17h4" />
    </>
  ),
  search: (
    <>
      <circle cx="10.5" cy="10.5" r="6.5" />
      <path d="m16 16 4.5 4.5" />
    </>
  ),
  keyboard: (
    <>
      <rect x="3" y="6" width="18" height="12" rx="3" />
      <path d="M7 10h.01M10.5 10h.01M14 10h.01M17.5 10h.01M7 14h.01M10.5 14h7" />
    </>
  ),
  pin: (
    <>
      <path d="m9 3 6 6M8 8l8 8M14 4l6 6-4 2-4 4-2 4-6-6 4-2 4-4 2-4Z" />
      <path d="m8 16-5 5" />
    </>
  ),
  mute: (
    <>
      <path d="M5 10v4h4l5 4V6l-5 4H5Z" />
      <path d="m18 9 4 4M22 9l-4 4" />
    </>
  ),
  trash: <path d="M4 7h16M9 3h6l1 4H8l1-4ZM7 7l1 14h8l1-14M10 11v6M14 11v6" />,
  chat: <path d="M5 18.5 3.5 21l.7-3.5A8 8 0 1 1 7 20l-2 .5Z" />,
  camera: (
    <>
      <path d="M4 7h4l2-3h4l2 3h4v13H4V7Z" />
      <circle cx="12" cy="13" r="4" />
    </>
  ),
  stop: <rect x="6" y="6" width="12" height="12" rx="2" />,
  tool: <path d="m14.5 4.5 5 5-9.5 9.5H5v-5l9.5-9.5ZM12 7l5 5" />,
  plug: (
    <>
      <path d="M9 3v4M15 3v4" />
      <path d="M6.5 7h11v3.5a5.5 5.5 0 0 1-11 0V7Z" />
      <path d="M12 16v5" />
    </>
  ),
};

export function Icon({ name, size = 22 }: { name: IconName; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {paths[name]}
    </svg>
  );
}
