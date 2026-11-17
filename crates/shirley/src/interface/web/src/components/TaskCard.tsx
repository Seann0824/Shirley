import { useEffect, useState } from "react";

import { api } from "../api/bridge";
import type { DeliveryFile, PoolTask, TaskStatus } from "../api/types";
import type { useTasks } from "../state/useTasks";
import { HtmlPreview, isHtml, openViewer, type ViewedFile } from "./FileViewer";
import { Icon, type IconName } from "./Icon";
import { MarkdownBlock, splitBlocks } from "./Markdown";

const STATUS_LABEL: Record<TaskStatus, string> = {
  backlog: "已搁置",
  to_do: "排队中",
  in_progress: "进行中",
  in_review: "做好了",
  blocked: "没做成",
  done: "已完成",
  cancelled: "已取消",
};

export function relativeTime(ms: number | null | undefined): string {
  if (!ms) return "";
  const minutes = Math.round((Date.now() - ms) / 60_000);
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  return `${Math.round(hours / 24)} 天前`;
}

function fileIcon(mime: string): IconName {
  if (mime.startsWith("image/")) return "image";
  if (mime.includes("html") || mime.includes("javascript") || mime.includes("json")) return "code";
  return "file";
}

/** The viewer's view of a delivered file. */
function viewed(bot: string, file: DeliveryFile): ViewedFile {
  return { bot, id: file.id, name: file.name, mime: file.mime };
}

function openFile(bot: string, file: DeliveryFile) {
  openViewer(viewed(bot, file));
}

/** A picture the bot made, shown in place. */
function ImagePreview({ bot, file }: { bot: string; file: DeliveryFile }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    api.assetUrl(bot, file.id).then((value) => alive && setUrl(value)).catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [bot, file.id]);
  return url ? (
    <img className="image-preview" src={url} alt={file.name} onClick={() => openFile(bot, file)} />
  ) : null;
}

function FileChip({ bot, file }: { bot: string; file: DeliveryFile }) {
  return (
    <button className="file-chip" onClick={() => openFile(bot, file)}>
      <Icon name={fileIcon(file.mime)} size={16} />
      <span>{file.name}</span>
    </button>
  );
}

/** What came back: pages and pictures shown, everything else as a file to open. */
function Deliverables({ bot, files }: { bot: string; files: DeliveryFile[] }) {
  const pages = files.filter((file) => isHtml(file.mime));
  const pictures = files.filter((file) => file.mime.startsWith("image/"));
  const others = files.filter((file) => !isHtml(file.mime) && !file.mime.startsWith("image/"));
  return (
    <>
      {pages.map((file) => (
        <HtmlPreview key={file.id} file={viewed(bot, file)} />
      ))}
      {pictures.map((file) => (
        <ImagePreview key={file.id} bot={bot} file={file} />
      ))}
      {others.length > 0 && (
        <div className="task-files">
          {others.map((file) => (
            <FileChip key={file.id} bot={bot} file={file} />
          ))}
        </div>
      )}
    </>
  );
}

/** A collapsed delivery note: the first paragraph, the rest on demand. */
function Summary({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const blocks = splitBlocks(text);
  // Collapsed, show enough to know what came back: a lone heading like "已交付" says nothing.
  let preview = 0;
  for (let length = 0; preview < blocks.length && length < 40; preview += 1) {
    length += blocks[preview].replace(/[*`#\-\s]/g, "").length;
  }
  const shown = open ? blocks : blocks.slice(0, Math.max(1, preview));
  return (
    <div className={`task-summary ${open ? "open" : ""}`}>
      {shown.map((block, index) => (
        <MarkdownBlock key={index} text={block} />
      ))}
      {blocks.length > shown.length || open ? (
        <button className="task-more" onClick={() => setOpen(!open)}>
          {open ? "收起" : "展开全部"}
        </button>
      ) : null}
    </div>
  );
}

/**
 * One piece of work a bot took on: where it stands, what came back, and the user's verdict.
 * The same card appears in the conversation that dispatched it and on the task page.
 */
export function TaskCard({
  task,
  botName,
  actions,
  showBot = false,
}: {
  task: PoolTask;
  botName?: string;
  actions: ReturnType<typeof useTasks>;
  showBot?: boolean;
}) {
  const [noting, setNoting] = useState(false);
  const [note, setNote] = useState("");
  const working = task.status === "in_progress" || task.status === "to_do";
  const time = relativeTime(task.finishedMs ?? task.startedMs ?? task.createdMs);
  const files = task.delivery?.files ?? [];

  const submitNote = () => {
    if (!note.trim()) return;
    void actions.note(task.id, note.trim());
    setNote("");
    setNoting(false);
  };

  return (
    <article className={`task-card ${task.status}`} id={`task-${task.id}`}>
      <header>
        <span className="task-state">
          {working ? <i className="task-spinner" /> : <Icon name={task.status === "blocked" ? "stop" : "check"} size={13} />}
          {STATUS_LABEL[task.status]}
          {showBot && botName ? ` · ${botName}` : ""}
          {time ? ` · ${time}` : ""}
          {task.attempt > 1 ? ` · 第 ${task.attempt} 次` : ""}
        </span>
        <h3>{task.title}</h3>
      </header>

      {task.delivery?.error && <p className="task-error">{task.delivery.error}</p>}
      {task.delivery?.summary && !task.delivery.error && <Summary text={task.delivery.summary} />}
      {working && task.note && <p className="task-note">按你的补充重做：{task.note}</p>}
      {task.pendingNotes && task.pendingNotes.length > 0 && (
        <p className="task-note">
          {task.status === "blocked" ? "重试时会加上：" : "做完这一版后会加上："}
          {task.pendingNotes.join("；")}
        </p>
      )}

      {files.length > 0 && <Deliverables bot={task.bot} files={files} />}

      {noting ? (
        <form
          className="task-note-form"
          onSubmit={(event) => {
            event.preventDefault();
            submitNote();
          }}
        >
          <input
            value={note}
            onChange={(event) => setNote(event.target.value)}
            placeholder={task.status === "in_review" || task.status === "done" ? "哪里要改？" : "要补充什么？"}
            autoFocus
          />
          <button type="submit" className="primary" disabled={!note.trim()}>
            发送
          </button>
          <button type="button" onClick={() => setNoting(false)}>
            取消
          </button>
        </form>
      ) : (
        <div className="task-actions">
          {task.status === "in_review" && (
            <>
              <button className="primary" onClick={() => void actions.accept(task.id)}>
                可以
              </button>
              <button onClick={() => setNoting(true)}>再改改</button>
            </>
          )}
          {working && (
            <>
              <button onClick={() => setNoting(true)}>补充</button>
              <button onClick={() => void actions.cancel(task.id)}>取消</button>
            </>
          )}
          {task.status === "blocked" && (
            <>
              <button className="primary" onClick={() => void actions.retry(task.id)}>
                重试
              </button>
              <button onClick={() => setNoting(true)}>补充后重做</button>
            </>
          )}
          {task.status === "done" && <button onClick={() => setNoting(true)}>再改改</button>}
          {task.status === "cancelled" && (
            <button onClick={() => void actions.remove(task.id)}>移除</button>
          )}
        </div>
      )}
    </article>
  );
}
