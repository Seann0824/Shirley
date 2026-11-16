import type { Bot, PoolTask, TaskStatus } from "../api/types";
import { BackHeader, LoadingRows } from "../components/Chrome";
import { Icon } from "../components/Icon";
import type { useTasks } from "../state/useTasks";

const STATUS_LABEL: Record<TaskStatus, string> = {
  backlog: "已搁置",
  to_do: "排队中",
  in_progress: "进行中",
  in_review: "待确认",
  blocked: "失败",
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

function TaskCard({
  task,
  bots,
  actions,
}: {
  task: PoolTask;
  bots: Bot[];
  actions: ReturnType<typeof useTasks>;
}) {
  const botName = bots.find((bot) => bot.name === task.bot)?.display_name ?? task.bot;
  const tone =
    task.status === "in_progress"
      ? "running"
      : task.status === "done"
        ? "done"
        : task.status === "in_review"
          ? "review"
          : task.status === "blocked"
            ? "failed"
            : "";
  const time = relativeTime(task.finishedMs ?? task.startedMs ?? task.createdMs);
  return (
    <article className={`queue-card ${tone}`}>
      {task.status === "in_progress" ? (
        <div className="queue-progress">
          <i />
        </div>
      ) : (
        <span className="queue-done">
          <Icon
            name={task.status === "done" ? "check" : task.status === "blocked" ? "stop" : "clock"}
            size={17}
          />
        </span>
      )}
      <div>
        <span className={task.status === "in_progress" ? "live-label" : ""}>
          {task.status === "in_progress" && <i />} {STATUS_LABEL[task.status]} · {botName}
        </span>
        <h2>{task.title}</h2>
        {task.delivery?.summary && <p className="delivery-summary">{task.delivery.summary}</p>}
        {task.delivery?.error && <p className="delivery-error">{task.delivery.error}</p>}
        {task.note && <p className="delivery-note">退回意见：{task.note}</p>}
        <p>
          {time}
          {task.attempt > 1 ? ` · 第 ${task.attempt} 次` : ""}
        </p>
        <div className="task-actions">
          {task.status === "in_review" && (
            <>
              <button className="primary" onClick={() => void actions.accept(task.id)}>
                接受
              </button>
              <button
                onClick={() => {
                  const note = window.prompt("哪里需要改？");
                  if (note !== null) void actions.reject(task.id, note);
                }}
              >
                退回
              </button>
            </>
          )}
          {task.status === "blocked" && (
            <>
              <button className="primary" onClick={() => void actions.retry(task.id)}>
                重试
              </button>
              <button onClick={() => void actions.remove(task.id)}>移除</button>
            </>
          )}
          {(task.status === "to_do" || task.status === "in_progress") && (
            <button onClick={() => void actions.cancel(task.id)}>取消</button>
          )}
          {(task.status === "done" || task.status === "cancelled") && (
            <button onClick={() => void actions.remove(task.id)}>移除</button>
          )}
        </div>
      </div>
    </article>
  );
}

export function Tasks({
  tasks,
  bots,
  onBack,
}: {
  tasks: ReturnType<typeof useTasks>;
  bots: Bot[];
  onBack: () => void;
}) {
  const groups: { title: string; statuses: TaskStatus[] }[] = [
    { title: "等你确认", statuses: ["in_review", "blocked"] },
    { title: "进行中", statuses: ["in_progress", "to_do", "backlog"] },
    { title: "已完成", statuses: ["done", "cancelled"] },
  ];
  return (
    <>
      <BackHeader title="任务" onBack={onBack} />
      {!tasks.loaded ? (
        <LoadingRows />
      ) : (
        <section className="task-queue content-ready">
          {tasks.error && <p className="error-note">{tasks.error}</p>}
          {tasks.tasks.length === 0 && <p className="empty-note">还没有任务。在首页把事情交给一个 Bot。</p>}
          {groups.map((group) => {
            const list = tasks.tasks.filter((task) => group.statuses.includes(task.status));
            if (list.length === 0) return null;
            return (
              <div className="task-section" key={group.title}>
                <h3>{group.title}</h3>
                {list.map((task) => (
                  <TaskCard key={task.id} task={task} bots={bots} actions={tasks} />
                ))}
              </div>
            );
          })}
        </section>
      )}
    </>
  );
}
