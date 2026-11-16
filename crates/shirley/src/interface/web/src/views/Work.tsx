import { api } from "../api/bridge";
import type { Bot, DeliveryFile, PoolTask } from "../api/types";
import { BackHeader, LoadingRows } from "../components/Chrome";
import { Icon, type IconName } from "../components/Icon";
import { relativeTime } from "./Tasks";

function fileIcon(mime: string): IconName {
  if (mime.startsWith("image/")) return "image";
  if (mime.startsWith("video/") || mime.startsWith("audio/")) return "spark";
  if (mime.includes("html") || mime.includes("javascript") || mime.includes("json")) return "code";
  return "file";
}

function open(bot: string, file: DeliveryFile) {
  api
    .assetUrl(bot, file.id)
    .then((url) => window.open(url, "_blank", "noopener"))
    .catch(() => undefined);
}

export function Work({
  tasks,
  loaded,
  bots,
  onBack,
}: {
  tasks: PoolTask[];
  loaded: boolean;
  bots: Bot[];
  onBack: () => void;
}) {
  const delivered = tasks
    .filter((task) => task.delivery && !task.delivery.error && (task.status === "done" || task.status === "in_review"))
    .sort((left, right) => (right.finishedMs ?? 0) - (left.finishedMs ?? 0));
  const monthStart = new Date();
  monthStart.setDate(1);
  monthStart.setHours(0, 0, 0, 0);
  const thisMonth = tasks.filter((task) => task.status === "done" && (task.finishedMs ?? 0) >= monthStart.getTime());
  const files = delivered.flatMap((task) => (task.delivery?.files ?? []).map((file) => ({ task, file })));

  return (
    <>
      <BackHeader title="交付" onBack={onBack} />
      {!loaded ? (
        <LoadingRows cards={3} />
      ) : (
        <div className="content-ready">
          <section className="work-stats">
            <div className="completion-ring">
              <span>{thisMonth.length}</span>
              <small>本月</small>
            </div>
            <div className="work-stats-copy">
              <strong>{delivered.length} 项交付</strong>
              <small>{files.length} 个文件</small>
            </div>
          </section>
          {delivered.length === 0 && <p className="empty-note">还没有交付物。完成的任务会出现在这里。</p>}
          <section className="delivery-list">
            {delivered.map((task) => {
              const botName = bots.find((bot) => bot.name === task.bot)?.display_name ?? task.bot;
              return (
                <article className="delivery-card" key={task.id}>
                  <header>
                    <strong>{task.title}</strong>
                    <small>
                      {botName} · {relativeTime(task.finishedMs)}
                    </small>
                  </header>
                  <p>{task.delivery?.summary}</p>
                  {task.delivery && task.delivery.files.length > 0 && (
                    <div className="asset-grid">
                      {task.delivery.files.map((file) => (
                        <button className="asset-card" key={file.id} onClick={() => open(task.bot, file)}>
                          <div className="asset-file-preview">
                            <Icon name={fileIcon(file.mime)} size={24} />
                          </div>
                          <p>{file.name}</p>
                          <small>{Math.max(1, Math.ceil(file.bytes / 1024))} KB</small>
                        </button>
                      ))}
                    </div>
                  )}
                </article>
              );
            })}
          </section>
        </div>
      )}
    </>
  );
}
