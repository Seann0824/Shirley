import { ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { type AgentEvent, getBridge } from "./bridge";

type IconName =
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
  | "camera";

function Icon({ name, size = 22 }: { name: IconName; size?: number }) {
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
    arrow: (
      <>
        <path d="M5 12h14M14 7l5 5-5 5" />
      </>
    ),
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
    bell: (
      <>
        <path d="M18 9a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9ZM10 21h4" />
      </>
    ),
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
    trash: (
      <>
        <path d="M4 7h16M9 3h6l1 4H8l1-4ZM7 7l1 14h8l1-14M10 11v6M14 11v6" />
      </>
    ),
    chat: <path d="M5 18.5 3.5 21l.7-3.5A8 8 0 1 1 7 20l-2 .5Z" />,
    camera: (
      <>
        <path d="M4 7h4l2-3h4l2 3h4v13H4V7Z" />
        <circle cx="12" cy="13" r="4" />
      </>
    ),
  };
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

type Tab = "home" | "tasks" | "work" | "memory" | "chat" | "new";

type CreatedBot = {
  name: string;
  avatar: string;
  description: string;
  skills: string[];
};

type Attachment = {
  id: string;
  name: string;
  kind: "image" | "pdf" | "file";
  url: string;
  size: string;
};

function filesToAttachments(files: FileList | null): Attachment[] {
  if (!files) return [];
  return Array.from(files).map((file) => ({
    id: `${file.name}-${file.lastModified}-${Math.random()}`,
    name: file.name,
    kind: file.type.startsWith("image/")
      ? "image"
      : file.type === "application/pdf" || file.name.toLowerCase().endsWith(".pdf")
        ? "pdf"
        : "file",
    url: URL.createObjectURL(file),
    size:
      file.size < 1024 * 1024
        ? `${Math.ceil(file.size / 1024)} KB`
        : `${(file.size / 1024 / 1024).toFixed(1)} MB`,
  }));
}

function StatusBar() {
  return (
    <div className="status-bar">
      <span>9:41</span>
      <div className="device-status">
        <i />
        <i />
        <b />
      </div>
    </div>
  );
}

function HomeHeader({ onNavigate }: { onNavigate: (view: Tab) => void }) {
  return (
    <header className="home-header">
      <button className="mini-avatar" onClick={() => onNavigate("memory")} aria-label="NONO 记忆">
        <span>
          <i />
          <i />
        </span>
        <b />
      </button>
      <strong>NONO</strong>
      <div className="header-actions">
        <button onClick={() => onNavigate("new")} aria-label="创建 Bot">
          <Icon name="plus" size={20} />
        </button>
        <button onClick={() => onNavigate("tasks")} aria-label="任务">
          <Icon name="clock" size={20} />
          <i />
        </button>
        <button onClick={() => onNavigate("work")} aria-label="交付">
          <Icon name="box" size={20} />
        </button>
      </div>
    </header>
  );
}

function BackHeader({ title, onBack }: { title: string; onBack: () => void }) {
  return (
    <header className="back-header">
      <button className="back-button" onClick={onBack} aria-label="返回">
        <Icon name="chevron" size={20} />
      </button>
      <strong>{title}</strong>
      <span className="header-spacer" />
    </header>
  );
}

function SwipeBotRow({
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
  const actionWidth = 174;
  const [open, setOpen] = useState(false);
  const [startX, setStartX] = useState<number | null>(null);
  const [offset, setOffset] = useState(0);
  const [dragged, setDragged] = useState(false);

  const close = () => {
    setOpen(false);
    setOffset(0);
  };

  return (
    <div className={`swipe-row ${pinned ? "pinned" : ""} ${muted ? "muted" : ""}`}>
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
          const base = open ? -actionWidth : 0;
          const delta = event.clientX - startX;
          if (Math.abs(delta) > 5) setDragged(true);
          setOffset(Math.max(-actionWidth, Math.min(0, base + delta)));
        }}
        onPointerUp={() => {
          const shouldOpen = offset < -62;
          setOpen(shouldOpen);
          setOffset(shouldOpen ? -actionWidth : 0);
          setStartX(null);
        }}
        onPointerCancel={() => {
          setOffset(open ? -actionWidth : 0);
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

function AttachmentTray({
  attachments,
  onRemove,
}: {
  attachments: Attachment[];
  onRemove: (id: string) => void;
}) {
  return (
    <div className="attachment-tray">
      {attachments.map((attachment) => (
        <div className={`attachment-preview ${attachment.kind}`} key={attachment.id}>
          {attachment.kind === "image" ? (
            <img src={attachment.url} alt="" />
          ) : (
            <span>
              <Icon name={attachment.kind === "pdf" ? "file" : "paperclip"} size={18} />
              <b>{attachment.kind === "pdf" ? "PDF" : "FILE"}</b>
            </span>
          )}
          <div>
            <strong>{attachment.name}</strong>
            <small>{attachment.size}</small>
          </div>
          <button onClick={() => onRemove(attachment.id)} aria-label={`移除 ${attachment.name}`}>
            ×
          </button>
        </div>
      ))}
    </div>
  );
}

function AttachmentSheet({
  onClose,
  onFiles,
}: {
  onClose: () => void;
  onFiles: (files: FileList | null) => void;
}) {
  const handleFiles = (files: FileList | null) => {
    onFiles(files);
    onClose();
  };

  return (
    <div className="attachment-sheet-layer" role="dialog" aria-modal="true" aria-label="添加内容">
      <button className="sheet-backdrop" onClick={onClose} aria-label="关闭" />
      <div className="attachment-sheet">
        <div className="sheet-handle" />
        <div className="attachment-options">
          <label>
            <span>
              <Icon name="image" size={22} />
            </span>
            <strong>照片</strong>
            <input
              type="file"
              accept="image/*"
              multiple
              onChange={(event) => handleFiles(event.target.files)}
            />
          </label>
          <label>
            <span>
              <Icon name="camera" size={22} />
            </span>
            <strong>拍摄</strong>
            <input
              type="file"
              accept="image/*"
              capture="environment"
              onChange={(event) => handleFiles(event.target.files)}
            />
          </label>
          <label>
            <span>
              <Icon name="folder" size={22} />
            </span>
            <strong>文件</strong>
            <input
              type="file"
              multiple
              accept=".pdf,.txt,.doc,.docx,.xls,.xlsx,.ppt,.pptx"
              onChange={(event) => handleFiles(event.target.files)}
            />
          </label>
          <label>
            <span>
              <Icon name="file" size={22} />
            </span>
            <strong>扫描</strong>
            <input
              type="file"
              accept="image/*,.pdf"
              capture="environment"
              onChange={(event) => handleFiles(event.target.files)}
            />
          </label>
        </div>
        <button className="sheet-cancel" onClick={onClose}>
          取消
        </button>
      </div>
    </div>
  );
}

function ComposerDock({
  value,
  onValueChange,
  onSend,
  onListeningChange,
}: {
  value: string;
  onValueChange: (value: string) => void;
  onSend: (text: string, attachments: Attachment[]) => void;
  onListeningChange?: (listening: boolean) => void;
}) {
  const [textMode, setTextMode] = useState(false);
  const [listening, setListening] = useState(false);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [showAttachmentSheet, setShowAttachmentSheet] = useState(false);

  const updateListening = (next: boolean) => {
    setListening(next);
    onListeningChange?.(next);
  };

  const send = (text: string) => {
    if (!text.trim() && !attachments.length) return;
    onSend(text.trim(), attachments);
    onValueChange("");
    setAttachments([]);
    setTextMode(false);
  };

  return (
    <>
      <div className={`composer-dock ${listening ? "is-listening" : ""}`}>
        {attachments.length > 0 && (
          <AttachmentTray
            attachments={attachments}
            onRemove={(id) => setAttachments((items) => items.filter((item) => item.id !== id))}
          />
        )}
        <div className="composer-controls">
          {textMode ? (
            <form
              className="unified-text-composer"
              onSubmit={(event) => {
                event.preventDefault();
                send(value);
              }}
            >
              <button type="button" onClick={() => setTextMode(false)} aria-label="切换语音">
                <Icon name="mic" size={20} />
              </button>
              <input
                value={value}
                onChange={(event) => onValueChange(event.target.value)}
                placeholder={attachments.length ? "添加说明…" : "输入消息"}
                autoFocus
              />
              <button
                type="submit"
                className="unified-send"
                disabled={!value.trim() && !attachments.length}
                aria-label="发送"
              >
                <Icon name="arrow" size={18} />
              </button>
            </form>
          ) : (
            <>
              <button
                className="composer-side"
                onClick={() => setShowAttachmentSheet(true)}
                aria-label="添加附件"
              >
                <Icon name="plus" size={20} />
              </button>
              <button
                className="composer-voice"
                onPointerDown={() => updateListening(true)}
                onPointerUp={() => {
                  updateListening(false);
                  send("请处理我刚才说的内容");
                }}
                onPointerCancel={() => updateListening(false)}
                onPointerLeave={() => updateListening(false)}
                aria-label="按住说话"
              >
                <span className="button-wave" aria-hidden="true">
                  <i />
                  <i />
                  <i />
                  <i />
                  <i />
                </span>
                <strong>{listening ? "松开发送" : "按住说话"}</strong>
              </button>
              <button
                className="composer-side"
                onClick={() => setTextMode(true)}
                aria-label="文字输入"
              >
                <Icon name="keyboard" size={20} />
              </button>
            </>
          )}
        </div>
      </div>
      {showAttachmentSheet && (
        <AttachmentSheet
          onClose={() => setShowAttachmentSheet(false)}
          onFiles={(files) => setAttachments((items) => [...items, ...filesToAttachments(files)])}
        />
      )}
    </>
  );
}

function Home({
  task,
  setTask,
  activeTask,
  assignedBot,
  onSendTask,
  onNavigate,
  onOpenBot,
  createdBots,
  showRoute,
  pinnedBots,
  onTogglePin,
  mutedBots,
  hiddenBots,
  onToggleMute,
  onDeleteBot,
}: {
  task: string;
  setTask: (value: string) => void;
  activeTask: string;
  assignedBot: string;
  onSendTask: (task: string) => void;
  onNavigate: (view: Tab) => void;
  onOpenBot: (bot: string) => void;
  createdBots: CreatedBot[];
  showRoute: boolean;
  pinnedBots: string[];
  onTogglePin: (bot: string) => void;
  mutedBots: string[];
  hiddenBots: string[];
  onToggleMute: (bot: string) => void;
  onDeleteBot: (bot: string) => void;
}) {
  return (
    <>
      <HomeHeader onNavigate={onNavigate} />

      {activeTask && showRoute && (
        <section className="route-result" role="status" aria-label={`任务已发送给 ${assignedBot}`}>
          <div className="route-symbol">
            <span>
              <Icon name="chat" size={14} />
            </span>
          </div>
          <div className="dispatch-track">
            <i />
          </div>
          <div className="dispatch-destination">
            <div className={`assigned-avatar ${assignedBot.toLowerCase()}`}>
              {assignedBot === "Patch" ? (
                <Icon name="code" size={18} />
              ) : assignedBot === "Scout" ? (
                <Icon name="globe" size={18} />
              ) : (
                <span>
                  <i />
                  <i />
                </span>
              )}
            </div>
            <strong>{assignedBot}</strong>
          </div>
        </section>
      )}

      <section className="home-bot-list">
        {!hiddenBots.includes("NONO") && (
          <SwipeBotRow
            name="NONO"
            className={`nono ${showRoute && assignedBot === "NONO" ? "receiving" : ""}`}
            pinned={pinnedBots.includes("NONO")}
            muted={mutedBots.includes("NONO")}
            onOpen={() => onOpenBot("NONO")}
            onTogglePin={() => onTogglePin("NONO")}
            onToggleMute={() => onToggleMute("NONO")}
            onDelete={() => onDeleteBot("NONO")}
          >
            <div className="home-bot-avatar">
              <span>
                <i />
                <i />
              </span>
              <b />
            </div>
            <div className="home-bot-info">
              <div>
                <strong>NONO</strong>
                <span className="status busy">
                  <i /> 工作中
                </span>
              </div>
              <div className="capability-images">
                {["web", "files", "create"].map((item) => (
                  <img key={item} src={`/capabilities/${item}.svg`} alt="" />
                ))}
              </div>
            </div>
          </SwipeBotRow>
        )}

        {!hiddenBots.includes("Patch") && (
          <SwipeBotRow
            name="Patch"
            className={`patch ${showRoute && assignedBot === "Patch" ? "receiving" : ""}`}
            pinned={pinnedBots.includes("Patch")}
            muted={mutedBots.includes("Patch")}
            onOpen={() => onOpenBot("Patch")}
            onTogglePin={() => onTogglePin("Patch")}
            onToggleMute={() => onToggleMute("Patch")}
            onDelete={() => onDeleteBot("Patch")}
          >
            <div className="home-bot-avatar">
              <Icon name="code" size={25} />
              <b />
            </div>
            <div className="home-bot-info">
              <div>
                <strong>Patch</strong>
                <span className="status">
                  <i /> 空闲
                </span>
              </div>
              <div className="bot-visual-strip code-strip">
                <span>&lt;/&gt;</span>
                <i />
                <i />
              </div>
            </div>
          </SwipeBotRow>
        )}

        {!hiddenBots.includes("Scout") && (
          <SwipeBotRow
            name="Scout"
            className={`scout ${showRoute && assignedBot === "Scout" ? "receiving" : ""}`}
            pinned={pinnedBots.includes("Scout")}
            muted={mutedBots.includes("Scout")}
            onOpen={() => onOpenBot("Scout")}
            onTogglePin={() => onTogglePin("Scout")}
            onToggleMute={() => onToggleMute("Scout")}
            onDelete={() => onDeleteBot("Scout")}
          >
            <div className="home-bot-avatar">
              <Icon name="globe" size={25} />
              <b />
            </div>
            <div className="home-bot-info">
              <div>
                <strong>Scout</strong>
                <span className="status">
                  <i /> 空闲
                </span>
              </div>
              <div className="bot-visual-strip web-strip">
                <Icon name="globe" size={16} />
                <i />
                <Icon name="search" size={15} />
              </div>
            </div>
          </SwipeBotRow>
        )}

        {createdBots
          .filter((bot) => !hiddenBots.includes(bot.name))
          .map((bot) => (
            <SwipeBotRow
              key={bot.name}
              name={bot.name}
              className={`custom-bot ${showRoute && assignedBot === bot.name ? "receiving" : ""}`}
              pinned={pinnedBots.includes(bot.name)}
              muted={mutedBots.includes(bot.name)}
              onOpen={() => onOpenBot(bot.name)}
              onTogglePin={() => onTogglePin(bot.name)}
              onToggleMute={() => onToggleMute(bot.name)}
              onDelete={() => onDeleteBot(bot.name)}
            >
              <div className="home-bot-avatar custom-avatar">
                <img src={bot.avatar} alt="" />
                <b />
              </div>
              <div className="home-bot-info">
                <div>
                  <strong>{bot.name}</strong>
                  <span className="status">
                    <i /> 空闲
                  </span>
                </div>
                <div className="bot-visual-strip custom-strip">
                  {bot.skills.filter((skill) => skill !== "adaptive").length ? (
                    bot.skills
                      .filter((skill) => skill !== "adaptive")
                      .slice(0, 3)
                      .map((skill) => {
                        const preset = initialSkills.find((item) => item.id === skill);
                        return preset ? <Icon key={skill} name={preset.icon} size={14} /> : null;
                      })
                  ) : (
                    <>
                      <i />
                      <i />
                      <i />
                    </>
                  )}
                </div>
              </div>
            </SwipeBotRow>
          ))}
      </section>

      <ComposerDock
        value={task}
        onValueChange={setTask}
        onSend={(text, attachments) =>
          onSendTask(text || (attachments.length ? "处理这些附件" : ""))
        }
      />
    </>
  );
}

const presetAvatars = [
  "/avatars/nono-classic.svg",
  "/avatars/nono-focus.svg",
  "/avatars/nono-scout.svg",
  "/avatars/nono-maker.svg",
  "/avatars/nono-calm.svg",
  "/avatars/nono-spark.svg",
];

const initialSkills = [
  { id: "search", label: "搜索", icon: "globe" as IconName },
  { id: "files", label: "文件", icon: "folder" as IconName },
  { id: "create", label: "创作", icon: "image" as IconName },
  { id: "code", label: "代码", icon: "code" as IconName },
];

function NewBotView({
  onBack,
  onCreate,
}: {
  onBack: () => void;
  onCreate: (bot: CreatedBot) => void;
}) {
  const [brief, setBrief] = useState("");
  const [request, setRequest] = useState("");
  const [installed, setInstalled] = useState(false);
  const [customAvatar, setCustomAvatar] = useState("");

  const chooseImage = (file?: File) => {
    if (!file) return;
    if (!file.type.startsWith("image/")) return;
    setCustomAvatar(URL.createObjectURL(file));
  };

  const matchedSkills = [
    /搜索|研究|调研|资料|竞品|跟踪/.test(request) ? "search" : "",
    /文件|整理|文档|报告|表格|会议/.test(request) ? "files" : "",
    /图片|视频|设计|创作|视觉/.test(request) ? "create" : "",
    /代码|开发|网站|应用|修复/.test(request) ? "code" : "",
  ].filter(Boolean);

  const skills = matchedSkills.length ? matchedSkills : ["search", "files"];
  const primarySkill = skills[0];
  const generatedName =
    primarySkill === "code"
      ? "Builder"
      : primarySkill === "create"
        ? "Muse"
        : primarySkill === "search"
          ? "Insight"
          : "Clerk";
  const generatedAvatar =
    customAvatar ||
    (primarySkill === "code"
      ? presetAvatars[1]
      : primarySkill === "create"
        ? presetAvatars[5]
        : primarySkill === "search"
          ? presetAvatars[2]
          : presetAvatars[4]);

  useEffect(() => {
    if (!request) return;
    setInstalled(false);
    const timer = window.setTimeout(() => setInstalled(true), 1300);
    return () => window.clearTimeout(timer);
  }, [request]);

  return (
    <>
      <BackHeader title="新 Bot" onBack={onBack} />
      <section className="bot-builder-chat">
        <div className="builder-messages">
          <div className="builder-message ai">
            <span className="builder-avatar">
              <Icon name="bot" size={17} />
            </span>
            <p>告诉我你需要一个怎样的 Bot。</p>
          </div>

          {request && (
            <>
              <div className="builder-message user">
                <p>{request}</p>
              </div>
              <div className="builder-message ai setup">
                <label className="generated-avatar" htmlFor="generated-avatar-upload">
                  <img src={generatedAvatar} alt="" />
                  <span>
                    <Icon name="image" size={12} />
                  </span>
                  <input
                    id="generated-avatar-upload"
                    type="file"
                    accept="image/*"
                    onChange={(event) => chooseImage(event.target.files?.[0])}
                  />
                </label>
                <div className="generated-bot">
                  <strong>{generatedName}</strong>
                  <div className="matched-skills">
                    {skills.map((skill) => {
                      const preset = initialSkills.find((item) => item.id === skill);
                      return preset ? (
                        <span key={skill}>
                          <Icon name={preset.icon} size={14} />
                        </span>
                      ) : null;
                    })}
                  </div>
                  <div className={`install-state ${installed ? "done" : ""}`}>
                    {installed ? <Icon name="check" size={13} /> : <i />}
                    <span>{installed ? "技能已安装" : "正在匹配技能库"}</span>
                  </div>
                </div>
              </div>
            </>
          )}
        </div>

        {installed ? (
          <div className="builder-confirm">
            <button
              className="revise-button"
              onClick={() => {
                setRequest("");
                setInstalled(false);
              }}
            >
              重新描述
            </button>
            <button
              className="create-bot-button"
              onClick={() =>
                onCreate({
                  name: generatedName,
                  avatar: generatedAvatar,
                  description: request,
                  skills: [...skills, "adaptive"],
                })
              }
            >
              创建 {generatedName}
            </button>
          </div>
        ) : (
          <form
            className="builder-composer"
            onSubmit={(event) => {
              event.preventDefault();
              if (!brief.trim()) return;
              setRequest(brief.trim());
              setBrief("");
            }}
          >
            <input
              value={brief}
              onChange={(event) => setBrief(event.target.value)}
              placeholder="例如：帮我跟踪竞品并整理周报"
              aria-label="描述 Bot 能力"
            />
            <button type="submit" disabled={!brief.trim()} aria-label="发送">
              <Icon name="arrow" size={18} />
            </button>
          </form>
        )}
      </section>
    </>
  );
}

type ChatMessage =
  | { role: "user"; text: string; attachments: Attachment[] }
  | { role: "assistant"; text: string; streaming: boolean };

function ChatView({
  bot,
  onBack,
  onMemory,
}: {
  bot: string;
  onBack: () => void;
  onMemory: () => void;
}) {
  const [listening, setListening] = useState(false);
  const [message, setMessage] = useState("");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [streaming, setStreaming] = useState(false);
  const sessionRef = useRef<string | undefined>(undefined);
  const unsubRef = useRef<(() => void) | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  // bot name as server project id (lowercase slug)
  const projectId = bot.toLowerCase();

  useEffect(() => {
    const bridge = getBridge();
    // Start SSE connection if not already running
    bridge.connect();

    const handler = (event: AgentEvent) => {
      if (event.project && event.project !== projectId) return;

      if (event.type === "content_delta") {
        setMessages((prev) => {
          const last = prev[prev.length - 1];
          if (last?.role === "assistant" && last.streaming) {
            return [
              ...prev.slice(0, -1),
              { ...last, text: last.text + event.text },
            ];
          }
          return [...prev, { role: "assistant", text: event.text, streaming: true }];
        });
      } else if (event.type === "finished" || event.type === "error") {
        setStreaming(false);
        setMessages((prev) => {
          const last = prev[prev.length - 1];
          if (last?.role === "assistant" && last.streaming) {
            const text = event.type === "error"
              ? last.text || `错误：${event.message}`
              : last.text || "（已完成）";
            return [...prev.slice(0, -1), { role: "assistant", text, streaming: false }];
          }
          return prev;
        });
      }
    };

    bridge.onEvent = handler;
    unsubRef.current = () => { bridge.onEvent = null; };

    return () => {
      unsubRef.current?.();
    };
  }, [projectId]);

  // Auto-scroll on new messages
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [messages]);

  const sendMessage = useCallback(async (text: string, attachments: Attachment[]) => {
    if (!text.trim() && !attachments.length) return;
    setMessages((prev) => [...prev, { role: "user", text: text.trim(), attachments }]);
    setStreaming(true);
    try {
      await getBridge().send(projectId, sessionRef.current, text.trim());
    } catch {
      setStreaming(false);
      setMessages((prev) => [
        ...prev,
        { role: "assistant", text: "连接失败，请检查服务器是否运行。", streaming: false },
      ]);
    }
  }, [projectId]);

  const botIcon =
    bot === "Patch" ? (
      <Icon name="code" size={27} />
    ) : bot === "Scout" ? (
      <Icon name="globe" size={27} />
    ) : (
      <span>
        <i />
        <i />
      </span>
    );

  return (
    <section className={`chat-view ${listening ? "is-listening" : ""}`}>
      <header className="chat-header">
        <button className="back-button" onClick={onBack} aria-label="返回">
          <Icon name="chevron" size={20} />
        </button>
        <div className={`chat-identity ${bot.toLowerCase()}`}>
          <div>{botIcon}</div>
          <span>
            <strong>{bot}</strong>
            <i />
          </span>
        </div>
        <button className="memory-button" onClick={onMemory} aria-label={`${bot} 的记忆`}>
          <Icon name="brain" size={19} />
        </button>
      </header>

      <div className="conversation" ref={scrollRef}>
        {listening ? (
          <div className="listening-state">
            <div className="voice-wave">
              {[16, 29, 45, 26, 54, 38, 62, 34, 48, 22, 39, 17].map((height, index) => (
                <i key={index} style={{ height }} />
              ))}
            </div>
            <strong>松开发送</strong>
          </div>
        ) : (
          <div className="message-thread">
            <span className="thread-time">今天</span>
            <div className="message-row bot-message">
              <div className={`message-avatar ${bot.toLowerCase()}`}>{botIcon}</div>
              <div className="message-bubble">
                <p>我在。直接告诉我需要完成什么。</p>
              </div>
            </div>
            {messages.map((msg, index) =>
              msg.role === "user" ? (
                <div className="message-row user-message" key={index}>
                  <div className="message-bubble multimodal-message">
                    {msg.attachments.length > 0 && (
                      <div className="sent-attachments">
                        {msg.attachments.map((attachment) =>
                          attachment.kind === "image" ? (
                            <img key={attachment.id} src={attachment.url} alt={attachment.name} />
                          ) : (
                            <div className="sent-file" key={attachment.id}>
                              <Icon name="file" size={19} />
                              <span>
                                <strong>{attachment.name}</strong>
                                <small>{attachment.size}</small>
                              </span>
                            </div>
                          ),
                        )}
                      </div>
                    )}
                    {msg.text && <p>{msg.text}</p>}
                  </div>
                </div>
              ) : (
                <div className="message-row bot-message" key={index}>
                  <div className={`message-avatar ${bot.toLowerCase()}`}>{botIcon}</div>
                  {msg.streaming && !msg.text ? (
                    <div className="message-bubble typing-bubble" aria-label={`${bot} 正在输入`}>
                      <i />
                      <i />
                      <i />
                    </div>
                  ) : (
                    <div className={`message-bubble ${msg.streaming ? "streaming" : ""}`}>
                      <p style={{ whiteSpace: "pre-wrap" }}>{msg.text}</p>
                    </div>
                  )}
                </div>
              ),
            )}
            {streaming && messages[messages.length - 1]?.role === "user" && (
              <div className="message-row bot-message">
                <div className={`message-avatar ${bot.toLowerCase()}`}>{botIcon}</div>
                <div className="message-bubble typing-bubble" aria-label={`${bot} 正在输入`}>
                  <i />
                  <i />
                  <i />
                </div>
              </div>
            )}
          </div>
        )}
      </div>

      <ComposerDock
        value={message}
        onValueChange={setMessage}
        onSend={sendMessage}
        onListeningChange={setListening}
      />
    </section>
  );
}

function useSettled(delay = 520) {
  const [settled, setSettled] = useState(false);

  useEffect(() => {
    const timer = window.setTimeout(() => setSettled(true), delay);
    return () => window.clearTimeout(timer);
  }, [delay]);

  return settled;
}

function LoadingRows({ cards = 2 }: { cards?: number }) {
  return (
    <div className="loading-rows" aria-label="正在加载">
      {Array.from({ length: cards }).map((_, index) => (
        <div className="loading-card" key={index}>
          <i />
          <span>
            <b />
            <b />
          </span>
        </div>
      ))}
      <p>
        <span />
        正在同步
      </p>
    </div>
  );
}

function TasksView({ task, bot, onBack }: { task: string; bot: string; onBack: () => void }) {
  const settled = useSettled();

  return (
    <>
      <BackHeader title="任务" onBack={onBack} />
      {!settled ? (
        <LoadingRows />
      ) : (
        <section className="task-queue content-ready">
          <article className="queue-card running">
            <div className="queue-progress">
              <span>62</span>
            </div>
            <div>
              <span className="live-label">
                <i /> RUNNING · {bot || "NONO"}
              </span>
              <h2>{task || "整理本周项目进度并生成报告"}</h2>
              <p>正在使用 2 个工具</p>
            </div>
            <Icon name="chevron" size={17} />
          </article>
          <article className="queue-card done">
            <span className="queue-done">
              <Icon name="check" size={17} />
            </span>
            <div>
              <span>DONE · SCOUT</span>
              <h2>竞品功能与定价调研</h2>
              <p>2 小时前完成</p>
            </div>
            <Icon name="chevron" size={17} />
          </article>
        </section>
      )}
    </>
  );
}

function WorkView({ onBack }: { onBack: () => void }) {
  const settled = useSettled(620);

  return (
    <>
      <BackHeader title="交付" onBack={onBack} />
      {!settled ? (
        <LoadingRows cards={3} />
      ) : (
        <div className="content-ready">
          <section className="work-stats">
            <div className="completion-ring">
              <span>18</span>
              <small>本月</small>
            </div>
            <div className="bar-chart">
              {[30, 52, 38, 76, 59, 92, 68].map((height, index) => (
                <i key={index} style={{ height: `${height}%` }} />
              ))}
            </div>
            <span className="success-badge">
              <Icon name="check" size={13} /> 100%
            </span>
          </section>
          <section className="asset-grid">
            <button className="asset-card asset-report">
              <div className="report-preview big">
                <span />
                <span />
                <div>
                  <i />
                  <i />
                  <i />
                  <i />
                </div>
              </div>
              <p>项目复盘</p>
              <small>PDF</small>
            </button>
            <button className="asset-card asset-image">
              <div className="visual-preview big">
                <i />
                <i />
                <Icon name="spark" size={24} />
              </div>
              <p>品牌视觉</p>
              <small>12 张图片</small>
            </button>
            <button className="asset-card asset-code">
              <div className="terminal-preview">
                <span>›_</span>
                <i />
                <i />
                <i />
              </div>
              <p>性能修复</p>
              <small>8 个文件</small>
            </button>
            <button className="asset-card asset-sheet">
              <div className="sheet-preview">
                <i />
                <i />
                <i />
                <i />
                <i />
                <i />
              </div>
              <p>数据分析</p>
              <small>XLSX</small>
            </button>
          </section>
        </div>
      )}
    </>
  );
}

function MemoryView({ bot, onBack }: { bot: string; onBack: () => void }) {
  return (
    <>
      <BackHeader title={bot} onBack={onBack} />
      <section className="memory-visual">
        <div className="memory-core">
          <Icon name="brain" size={34} />
          <span className="node n1" />
          <span className="node n2" />
          <span className="node n3" />
          <span className="node n4" />
        </div>
        <h1>{bot} 记得</h1>
        <p>12 条记忆正在生效</p>
      </section>
      <section className="memory-map">
        <button className="memory-block personal">
          <span className="memory-icon">
            <Icon name="bot" size={23} />
          </span>
          <div className="memory-lines">
            <i />
            <i />
            <i />
          </div>
          <strong>关于你</strong>
          <small>6</small>
        </button>
        <button className="memory-block project">
          <span className="memory-icon">
            <Icon name="folder" size={23} />
          </span>
          <div className="memory-lines">
            <i />
            <i />
            <i />
          </div>
          <strong>当前项目</strong>
          <small>6</small>
        </button>
      </section>
      <div className="privacy-pill">
        <Icon name="shield" size={16} />
        <span>仅你和 {bot} 可见</span>
        <i />
      </div>
    </>
  );
}

export default function App() {
  const [tab, setTab] = useState<Tab>("home");
  const [task, setTask] = useState("");
  const [activeTask, setActiveTask] = useState("");
  const [assignedBot, setAssignedBot] = useState("");
  const [selectedBot, setSelectedBot] = useState("NONO");
  const [createdBots, setCreatedBots] = useState<CreatedBot[]>([]);
  const [showRoute, setShowRoute] = useState(false);
  const [pinnedBots, setPinnedBots] = useState<string[]>(["NONO"]);
  const [mutedBots, setMutedBots] = useState<string[]>([]);
  const [hiddenBots, setHiddenBots] = useState<string[]>([]);
  const [routeVersion, setRouteVersion] = useState(0);

  useEffect(() => {
    if (!routeVersion) return;
    setShowRoute(true);
    const timer = window.setTimeout(() => setShowRoute(false), 2600);
    return () => window.clearTimeout(timer);
  }, [routeVersion]);

  const submitTask = (nextTask: string) => {
    if (!nextTask.trim()) return;
    const bot = /代码|开发|修复|网站|应用|bug|构建/i.test(nextTask)
      ? "Patch"
      : /搜索|查找|研究|调研|网页|资料|竞品/i.test(nextTask)
        ? "Scout"
        : "NONO";
    setActiveTask(nextTask);
    setAssignedBot(bot);
    setRouteVersion((current) => current + 1);
  };

  const openBot = (bot: string) => {
    setSelectedBot(bot);
    setTab("chat");
  };

  const togglePin = (bot: string) => {
    setPinnedBots((current) =>
      current.includes(bot) ? current.filter((item) => item !== bot) : [...current, bot],
    );
  };

  const toggleMute = (bot: string) => {
    setMutedBots((current) =>
      current.includes(bot) ? current.filter((item) => item !== bot) : [...current, bot],
    );
  };

  const deleteBot = (bot: string) => {
    setHiddenBots((current) => [...current, bot]);
    setPinnedBots((current) => current.filter((item) => item !== bot));
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
                task={task}
                setTask={setTask}
                activeTask={activeTask}
                assignedBot={assignedBot}
                onSendTask={submitTask}
                onNavigate={setTab}
                onOpenBot={openBot}
                createdBots={createdBots}
                showRoute={showRoute}
                pinnedBots={pinnedBots}
                onTogglePin={togglePin}
                mutedBots={mutedBots}
                hiddenBots={hiddenBots}
                onToggleMute={toggleMute}
                onDeleteBot={deleteBot}
              />
            )}
            {tab === "tasks" && (
              <TasksView task={activeTask} bot={assignedBot} onBack={() => setTab("home")} />
            )}
            {tab === "work" && <WorkView onBack={() => setTab("home")} />}
            {tab === "chat" && (
              <ChatView
                bot={selectedBot}
                onBack={() => setTab("home")}
                onMemory={() => setTab("memory")}
              />
            )}
            {tab === "memory" && <MemoryView bot={selectedBot} onBack={() => setTab("chat")} />}
            {tab === "new" && (
              <NewBotView
                onBack={() => setTab("home")}
                onCreate={(bot) => {
                  setCreatedBots((current) => [...current, bot]);
                  setTab("home");
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
