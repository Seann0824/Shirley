import type { Bot, TaskStatus } from "../api/types";
import { BackHeader, LoadingRows } from "../components/Chrome";
import { TaskCard } from "../components/TaskCard";
import type { useTasks } from "../state/useTasks";

export { relativeTime } from "../components/TaskCard";

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
          {tasks.tasks.length === 0 && <p className="empty-note">还没有任务。跟 bot 说要做的事，耗时的它会派出去做，结果会出现在这里和对话里。</p>}
          {groups.map((group) => {
            const list = tasks.tasks.filter((task) => group.statuses.includes(task.status));
            if (list.length === 0) return null;
            return (
              <div className="task-section" key={group.title}>
                <h3>{group.title}</h3>
                {list.map((task) => (
                  <TaskCard
                    key={task.id}
                    task={task}
                    botName={bots.find((bot) => bot.name === task.bot)?.display_name ?? task.bot}
                    actions={tasks}
                    showBot
                  />
                ))}
              </div>
            );
          })}
        </section>
      )}
    </>
  );
}
