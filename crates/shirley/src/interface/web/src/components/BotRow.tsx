import { useState, type ReactNode } from "react";

import { Icon } from "./Icon";

const ACTION_WIDTH = 174;

/** A bot card on the home list; swipe left for pin / mute / remove. */
export function SwipeBotRow({
  name,
  className,
  pinned,
  muted,
  onOpen,
  onTogglePin,
  onToggleMute,
  onDelete,
  children,
}: {
  name: string;
  className: string;
  pinned: boolean;
  muted: boolean;
  onOpen: () => void;
  onTogglePin: () => void;
  onToggleMute: () => void;
  onDelete: () => void;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [startX, setStartX] = useState<number | null>(null);
  const [offset, setOffset] = useState(0);
  const [dragged, setDragged] = useState(false);

  const close = () => {
    setOpen(false);
    setOffset(0);
  };

  return (
    <div
      className={`swipe-row ${pinned ? "pinned" : ""} ${muted ? "muted" : ""} ${open || offset !== 0 ? "revealing" : ""}`}
    >
      <div className="swipe-actions">
        <button
          className={pinned ? "active" : ""}
          onClick={() => {
            onTogglePin();
            close();
          }}
        >
          <Icon name="pin" size={18} />
          <span>{pinned ? "取消" : "置顶"}</span>
        </button>
        <button
          className={muted ? "active" : ""}
          onClick={() => {
            onToggleMute();
            close();
          }}
        >
          <Icon name="mute" size={18} />
          <span>{muted ? "恢复" : "静音"}</span>
        </button>
        <button
          className="destructive"
          onClick={() => {
            onDelete();
            close();
          }}
        >
          <Icon name="trash" size={18} />
          <span>移除</span>
        </button>
      </div>
      <button
        className={`home-bot-card ${className}`}
        style={{ transform: `translateX(${offset}px)` }}
        onPointerDown={(event) => {
          setStartX(event.clientX);
          setDragged(false);
          event.currentTarget.setPointerCapture(event.pointerId);
        }}
        onPointerMove={(event) => {
          if (startX === null) return;
          const base = open ? -ACTION_WIDTH : 0;
          const delta = event.clientX - startX;
          if (Math.abs(delta) > 5) setDragged(true);
          setOffset(Math.max(-ACTION_WIDTH, Math.min(0, base + delta)));
        }}
        onPointerUp={() => {
          const shouldOpen = offset < -62;
          setOpen(shouldOpen);
          setOffset(shouldOpen ? -ACTION_WIDTH : 0);
          setStartX(null);
        }}
        onPointerCancel={() => {
          setOffset(open ? -ACTION_WIDTH : 0);
          setStartX(null);
        }}
        onClick={() => {
          if (dragged) {
            setDragged(false);
            return;
          }
          if (open) close();
          else onOpen();
        }}
        aria-label={`${name}，左滑显示操作`}
      >
        {children}
      </button>
    </div>
  );
}
