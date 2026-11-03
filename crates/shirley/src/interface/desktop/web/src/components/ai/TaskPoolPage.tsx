import {
  Archive,
  Check,
  FileText,
  Loader2,
  Play,
  RotateCcw,
  Send,
  Trash2,
  Undo2,
  X,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  agentBridge,
  type DeliveryFile,
  type PoolTask,
  type PoolTaskStatus,
} from "@/lib/bridge";
import { AiMarkdown } from "@/components/ai/AiMarkdown";
import { Button } from "@/ui/button";
import { FIELD_SHELL } from "@/ui/field";
import { cn } from "@/ui/utils";

/**
 * The task **pool**, drawn as a board.
 *
 * This is the surface for the way of working where the transcript is not the product. A task is
 * one instruction; it runs in its own background session; what comes back is (a) a delivery note
 * saying what was built and how to check it and (b) the files that appeared while it ran. Nothing
 * enters `done` on its own -- a finished run parks in `in_review` and waits for a verdict. Sending
 * a delivery back is not a dead end either: the note becomes the next requirement and the same
 * session continues, so the agent keeps everything it already built.
 *
 * A board rather than a list, because the queue has more than one state at a time and the states
 * are the point: "how much is waiting on me" is a glance at the left columns, not a scroll through
 * a reversed log. Every column is always drawn, empty or not -- an empty column is the honest
 * answer ("nothing is blocked"), and the columns themselves are the workflow, so it needs no
 * legend. Only the happy path plus the two states a human owns are columns; nothing here is a
 * hidden tab that swallows a task.
 *
 * Cards stay small on purpose. The one heavy thing (judging a delivery: read the note, open the
 * files, accept or send back) happens in the detail panel on the right, so a board of twenty tasks
 * is still twenty one-line cards instead of twenty essays.
 *
 * Deliberately absent: a transcript. The process belongs to the agent; progress shows up as a
 * status, and when the user wants to look at how it went, the session is a normal session in the
 * sidebar. Adding a log pane here would recreate the thing this page exists to avoid.
 *
 * Refresh is polling, not a new event stream: the board is a handful of cards that change on the
 * order of minutes, and `AgentEvent` is already the contract for transcripts.
 */
export function TaskPoolPage({ project }: { project: string }) {
  const [tasks, setTasks] = useState<PoolTask[] | null>(null);
  const [error, setError] = useState("");
  const [brief, setBrief] = useState("");
  const [busy, setBusy] = useState(false);
  /** Card whose detail panel is open, or `null` when the board is unobstructed. */
  const [openId, setOpenId] = useState<string | null>(null);
  /** The note being typed in send-back mode, or `null` when the panel is not doing that. */
  const [rejecting, setRejecting] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const composeRef = useRef<HTMLTextAreaElement>(null);

  const load = useCallback(
    async (quiet = false) => {
      try {
        const bridge = await agentBridge();
        setTasks(await bridge.listPoolTasks(project));
        setError("");
      } catch (e) {
        if (!quiet) setTasks([]);
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [project],
  );

  useEffect(() => {
    void load();
  }, [load]);

  // A task that finishes while the page is open has to appear without a manual refresh. 2s is
  // frequent enough to feel live and cheap enough that it costs nothing while the pool is idle.
  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void load(true);
    }, 2000);
    return () => window.clearInterval(timer);
  }, [load]);

  // One ticker for the whole board: only the running column prints seconds, and one interval
  // beats one per card.
  const running = tasks?.some((task) => task.status === "in_progress") ?? false;
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [running]);

  /** Every mutation returns the whole list, so one snapshot always wins (no partial patches). */
  const mutate = useCallback(
    async (action: (bridge: Awaited<ReturnType<typeof agentBridge>>) => Promise<PoolTask[]>) => {
      setBusy(true);
      try {
        const bridge = await agentBridge();
        setTasks(await action(bridge));
        setError("");
        return true;
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
        return false;
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const enqueue = useCallback(async () => {
    const instruction = brief.trim();
    if (!instruction || busy) return;
    const ok = await mutate((bridge) => bridge.enqueuePoolTask(project, instruction));
    if (ok) {
      setBrief("");
      composeRef.current?.focus();
    }
  }, [brief, busy, mutate, project]);

  const openFile = useCallback(
    async (file: DeliveryFile) => {
      try {
        const bridge = await agentBridge();
        const url = await bridge.assetUrl(project, file.id);
        window.open(url, "_blank", "noreferrer");
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [project],
  );

  /** The panel follows the list: a task that leaves the pool takes its panel with it. */
  const openTask = (tasks ?? []).find((task) => task.id === openId) ?? null;

  const columns = COLUMNS.map((column) => ({
    ...column,
    items: (tasks ?? []).filter((task) => task.status === column.status),
  }));

  const transition = (id: string, status: PoolTaskStatus) =>
    void mutate((bridge) => bridge.transitionPoolTask(project, id, status));

  return (
    <div className="relative flex min-h-0 flex-1 flex-col overflow-hidden bg-canvas">
      <div className="flex shrink-0 flex-col gap-3 px-6 pt-7 pb-4">
        <header className="flex items-center justify-between gap-4">
          <div className="flex min-w-0 items-center gap-2">
            <h1 className="truncate font-display text-title-sm text-ink">任务池</h1>
            {tasks !== null && <span className="truncate text-caption text-muted">{summarize(tasks)}</span>}
          </div>
          {busy && <Loader2 className="size-4 shrink-0 animate-spin text-muted" />}
        </header>

        {/* One field, one key. The instruction is the whole contract of a task, so it gets the
            composer's own field shell rather than a dialog. */}
        <div className={cn(FIELD_SHELL, "flex flex-col gap-2 p-3")}>
          <textarea
            ref={composeRef}
            value={brief}
            onChange={(event) => setBrief(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                event.preventDefault();
                void enqueue();
              }
            }}
            rows={2}
            placeholder="要它交付什么？（写清验收标准；⌘/Ctrl+Enter 投放，可以连着投多个）"
            aria-label="New task"
            className="max-h-64 min-h-[3.25rem] w-full resize-y bg-transparent text-body-sm text-ink placeholder:text-muted focus:outline-hidden"
          />
          <div className="flex items-center justify-between gap-2">
            <span className="text-caption text-muted">
              投放即进「待办」，同时最多跑 {MAX_CONCURRENT} 个；跑完停在「待审核」，等你验收。
            </span>
            <Button
              size="sm"
              variant="primary"
              disabled={busy || !brief.trim()}
              onClick={() => void enqueue()}
            >
              <Send className="size-3.5" />
              投放任务
            </Button>
          </div>
        </div>

        {error && (
          <p className="rounded-control bg-danger-soft px-3 py-2 text-caption text-danger-ink">{error}</p>
        )}
      </div>

      {/* The board scrolls sideways on its own; the page never grows a scrollbar. */}
      <div className="flex min-h-0 flex-1 items-stretch gap-3 overflow-x-auto overflow-y-hidden px-6 pb-6">
        {tasks === null ? (
          <div className="flex flex-1 items-center justify-center gap-2 text-caption text-muted">
            <Loader2 className="size-4 animate-spin" />
            读取任务池…
          </div>
        ) : (
          columns.map((column) => (
            <section
              key={column.status}
              aria-label={column.label}
              className="flex w-[16.5rem] shrink-0 flex-col rounded-card border border-line bg-inset"
            >
              <header className="flex shrink-0 items-baseline justify-between gap-2 px-3 pt-3">
                <div className="flex min-w-0 items-center gap-1.5">
                  <span className={cn("size-1.5 shrink-0 rounded-full", COLUMN_DOT[column.status])} />
                  <h2
                    className={cn(
                      "truncate text-caption font-medium",
                      column.needsYou ? "text-ink" : "text-muted",
                    )}
                  >
                    {column.label}
                  </h2>
                </div>
                <span className="shrink-0 font-utility text-caption text-muted">{column.items.length}</span>
              </header>
              <p className="shrink-0 px-3 pt-0.5 pb-2 text-caption text-muted/60">{column.hint}</p>
              <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto px-2.5 pb-2.5">
                {column.items.length === 0 ? (
                  <p className="rounded-control border border-dashed border-line px-3 py-6 text-center text-caption text-muted/50">
                    空
                  </p>
                ) : (
                  column.items.map((task) => (
                    <TaskCard
                      key={task.id}
                      task={task}
                      now={now}
                      onOpen={() => {
                        setRejecting(null);
                        setOpenId(task.id);
                      }}
                    />
                  ))
                )}
              </div>
            </section>
          ))
        )}
      </div>

      {openTask && (
        <div className="absolute inset-0 z-20 flex justify-end">
          <div
            className="absolute inset-0 animate-overlay-in bg-ink/25"
            onClick={() => setOpenId(null)}
          />
          <TaskDetail
            task={openTask}
            now={now}
            busy={busy}
            rejecting={rejecting}
            onClose={() => setOpenId(null)}
            onOpenFile={(file) => void openFile(file)}
            onAccept={() => void mutate((bridge) => bridge.acceptPoolTask(project, openTask.id))}
            onReject={() => {
              if (rejecting === null) return;
              void mutate((bridge) =>
                bridge.rejectPoolTask(project, openTask.id, rejecting),
              ).then((ok) => {
                if (ok) setRejecting(null);
              });
            }}
            onStartReject={() => setRejecting("")}
            onCancelReject={() => setRejecting(null)}
            onNote={setRejecting}
            onRetry={() => void mutate((bridge) => bridge.retryPoolTask(project, openTask.id))}
            onTransition={(status) => transition(openTask.id, status)}
            onCancel={() => void mutate((bridge) => bridge.cancelPoolTask(project, openTask.id))}
            onRemove={() =>
              void mutate((bridge) => bridge.removePoolTask(project, openTask.id)).then(() =>
                setOpenId(null),
              )
            }
          />
        </div>
      )}
    </div>
  );
}

const MAX_CONCURRENT = 2;

const ICON_BUTTON =
  "flex size-8 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-quiet hover:text-ink";

const SECTION_LABEL = "text-caption font-medium tracking-wide text-muted uppercase";

/**
 * The workflow is Jira's, in Jira's own Chinese terms -- there is nothing new to learn, and
 * "blocked" is the honest home for a run that broke: it is waiting on a human, not finished.
 *
 * Order is "what needs you" first, which on a board also means "what is on screen first" when the
 * window is too narrow for seven columns: review, blocked, running, queued, parked, closed.
 */
const COLUMNS: {
  status: PoolTaskStatus;
  label: string;
  hint: string;
  /** The two states a human has to act on -- their headers stay at full contrast. */
  needsYou?: boolean;
}[] = [
  { status: "in_review", label: "待审核", hint: "跑完等你验收", needsYou: true },
  { status: "blocked", label: "被阻塞", hint: "跑挂了等你决定", needsYou: true },
  { status: "in_progress", label: "处理中", hint: `最多 ${MAX_CONCURRENT} 个并行` },
  { status: "to_do", label: "待办", hint: "排队等开工" },
  { status: "backlog", label: "积压", hint: "暂缓，不会自动开始" },
  { status: "done", label: "完成", hint: "你点过验收通过" },
  { status: "cancelled", label: "已取消", hint: "不做了" },
];

const COLUMN_DOT: Record<PoolTaskStatus, string> = {
  in_review: "bg-primary",
  blocked: "bg-danger-ink",
  in_progress: "bg-ink",
  to_do: "bg-line-strong",
  backlog: "bg-muted/50",
  done: "bg-muted/40",
  cancelled: "bg-muted/30",
};

const STATUS_LABEL: Record<PoolTaskStatus, string> = {
  in_review: "待审核",
  blocked: "被阻塞",
  in_progress: "处理中",
  to_do: "待办",
  backlog: "积压",
  done: "完成",
  cancelled: "已取消",
};

const STATUS_TONE: Record<PoolTaskStatus, string> = {
  in_review: "bg-primary/12 text-primary",
  blocked: "bg-danger-soft text-danger-ink",
  in_progress: "bg-inset text-ink",
  to_do: "bg-inset text-muted",
  backlog: "bg-quiet text-muted",
  done: "bg-quiet text-muted",
  cancelled: "bg-quiet text-muted",
};

/** The board's one-line status: what actually needs the user, then what is in flight. */
function summarize(tasks: PoolTask[]): string {
  if (tasks.length === 0) return "还没有任务";
  const pending = tasks.filter((task) => task.status === "in_review").length;
  const blocked = tasks.filter((task) => task.status === "blocked").length;
  const running = tasks.filter((task) => task.status === "in_progress").length;
  const queued = tasks.filter((task) => task.status === "to_do").length;
  const parts = [`${tasks.length} 个任务`];
  if (pending) parts.push(`${pending} 个待审核`);
  if (blocked) parts.push(`${blocked} 个被阻塞`);
  if (running) parts.push(`${running} 个在跑`);
  if (queued) parts.push(`${queued} 个待办`);
  return parts.join(" · ");
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatElapsed(from: number, to: number): string {
  const seconds = Math.max(0, Math.round((to - from) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m${seconds % 60 ? ` ${seconds % 60}s` : ""}`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

/**
 * A card is a handle, not a report: enough to recognise the task and to see whether it needs you.
 * Everything that takes reading lives in the panel behind it.
 */
function TaskCard({ task, now, onOpen }: { task: PoolTask; now: number; onOpen: () => void }) {
  // The title is the instruction's first line, so the rest of it is what the card adds.
  const rest = task.brief.split("\n").slice(1).join("\n").trim();
  const files = task.delivery?.files.length ?? 0;
  const failed = Boolean(task.delivery?.error);
  const elapsed =
    task.status === "in_progress" && task.startedMs !== null
      ? formatElapsed(task.startedMs, now)
      : null;

  return (
    <button
      type="button"
      onClick={onOpen}
      className={cn(
        "flex w-full flex-col gap-1.5 rounded-card border bg-surface p-3 text-left transition-colors",
        "border-line hover:border-line-strong hover:bg-quiet",
        task.status === "in_review" && "border-primary/40",
        task.status === "blocked" && "border-danger-soft",
      )}
    >
      <span className="line-clamp-2 text-body-sm font-medium text-ink">{task.title}</span>
      {rest && <span className="line-clamp-2 text-caption text-muted">{rest}</span>}
      <span className="flex flex-wrap items-center gap-x-2 gap-y-1 text-caption text-muted">
        {task.attempt > 1 && <span>第 {task.attempt} 轮</span>}
        {elapsed && <span>{elapsed}</span>}
        {files > 0 && (
          <span className="flex items-center gap-1">
            <FileText className="size-3" />
            {files}
          </span>
        )}
        {failed && <span className="text-danger-ink">没交出来</span>}
      </span>
    </button>
  );
}

/** The judging surface: the full instruction, what came back, and the one verdict it needs. */
function TaskDetail({
  task,
  now,
  busy,
  rejecting,
  onClose,
  onOpenFile,
  onAccept,
  onReject,
  onStartReject,
  onCancelReject,
  onNote,
  onRetry,
  onTransition,
  onCancel,
  onRemove,
}: {
  task: PoolTask;
  now: number;
  busy: boolean;
  /** The note being typed, or `null` when this panel is not sending the task back. */
  rejecting: string | null;
  onClose: () => void;
  onOpenFile: (file: DeliveryFile) => void;
  onAccept: () => void;
  onReject: () => void;
  onStartReject: () => void;
  onCancelReject: () => void;
  onNote: (note: string) => void;
  onRetry: () => void;
  onTransition: (status: PoolTaskStatus) => void;
  onCancel: () => void;
  onRemove: () => void;
}) {
  const delivery = task.delivery;
  const isSettled = task.status === "done" || task.status === "cancelled";

  return (
    <aside
      role="dialog"
      aria-modal="true"
      aria-label={task.title}
      className="relative flex h-full w-[26rem] max-w-[92%] animate-overlay-in flex-col border-l border-line bg-surface"
    >
      <header className="flex shrink-0 items-start justify-between gap-3 px-5 pt-5 pb-3">
        <div className="flex min-w-0 flex-col gap-1.5">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <span
              className={cn(
                "shrink-0 rounded-full px-2 py-0.5 font-utility text-caption",
                STATUS_TONE[task.status],
              )}
            >
              {STATUS_LABEL[task.status]}
            </span>
            {task.attempt > 1 && (
              <span className="shrink-0 text-caption text-muted">第 {task.attempt} 轮</span>
            )}
            {task.status === "in_progress" && task.startedMs !== null && (
              <span className="shrink-0 text-caption text-muted">
                {formatElapsed(task.startedMs, now)}
              </span>
            )}
          </div>
          <h2 className="text-body font-medium text-ink">{task.title}</h2>
        </div>
        <button type="button" aria-label="关闭" title="关闭" onClick={onClose} className={ICON_BUTTON}>
          <X className="size-4" />
        </button>
      </header>

      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-5 pb-4">
        {/* The instruction is shown in full: it is the definition of done, so it has to stay
            visible while judging, not hide behind an expander. */}
        <section className="flex flex-col gap-1.5">
          <h3 className={SECTION_LABEL}>指令</h3>
          <p className="whitespace-pre-wrap text-body-sm text-ink">{task.brief}</p>
        </section>

        {task.note && task.attempt > 1 && (
          <section className="flex flex-col gap-1.5">
            <h3 className={SECTION_LABEL}>退回意见</h3>
            <p className="rounded-control bg-quiet px-3 py-2 text-caption text-muted">{task.note}</p>
          </section>
        )}

        {task.status === "in_progress" && (
          <p className="flex items-center gap-2 text-caption text-muted">
            <Loader2 className="size-3.5 animate-spin" />
            在后台跑，不用等它——可以继续投放别的任务。
          </p>
        )}

        {task.status === "backlog" && (
          <p className="flex items-center gap-2 text-caption text-muted">
            <Archive className="size-3.5" />
            已暂缓：不会自动开始，点「开始」才回到待办。
          </p>
        )}

        {delivery?.error && (
          <section className="flex flex-col gap-1.5">
            <h3 className={SECTION_LABEL}>这一轮没交出来</h3>
            <p className="rounded-control bg-danger-soft px-3 py-2 text-caption text-danger-ink">
              {delivery.error}
            </p>
          </section>
        )}

        {delivery && !delivery.error && (
          <section className="flex flex-col gap-2">
            <h3 className={SECTION_LABEL}>交付说明</h3>
            <div className="text-body-sm text-ink">
              <AiMarkdown content={delivery.summary} streaming={false} />
            </div>
            {delivery.files.length > 0 && (
              <ul className="flex flex-col gap-1.5">
                {delivery.files.map((file) => (
                  <li key={file.id}>
                    <button
                      type="button"
                      onClick={() => onOpenFile(file)}
                      title={`打开 ${file.name}`}
                      className="flex w-full items-center gap-2 rounded-control border border-line bg-inset px-2.5 py-1.5 text-caption text-ink transition-colors hover:bg-selected"
                    >
                      <FileText className="size-3.5 shrink-0 text-muted" />
                      <span className="min-w-0 flex-1 truncate text-left">{file.name}</span>
                      <span className="shrink-0 text-muted">{formatBytes(file.bytes)}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>
        )}

        {rejecting !== null && (
          <section className="flex flex-col gap-2">
            <h3 className={SECTION_LABEL}>要它改什么</h3>
            <textarea
              value={rejecting}
              onChange={(event) => onNote(event.target.value)}
              rows={3}
              autoFocus
              placeholder="这条会变成下一轮的要求，同一段上下文继续做。"
              aria-label="退回意见"
              className={cn(
                FIELD_SHELL,
                "w-full resize-y px-3 py-2 text-body-sm text-ink placeholder:text-muted focus:outline-hidden",
              )}
            />
            <div className="flex items-center gap-2">
              <Button size="sm" variant="primary" disabled={busy || !rejecting.trim()} onClick={onReject}>
                <Play className="size-3.5" />
                退回并重做
              </Button>
              <Button size="sm" variant="ghost" onClick={onCancelReject}>
                取消
              </Button>
            </div>
          </section>
        )}
      </div>

      {/* Actions live here, one line, in the order of "what this state needs next". */}
      <footer className="flex shrink-0 flex-wrap items-center gap-2 border-t border-line px-5 py-3">
        {task.status === "in_review" && rejecting === null && (
          <>
            <Button size="sm" variant="primary" disabled={busy} onClick={onAccept}>
              <Check className="size-3.5" />
              验收通过
            </Button>
            <Button size="sm" variant="secondary" disabled={busy} onClick={onStartReject}>
              <Undo2 className="size-3.5" />
              退回重做
            </Button>
          </>
        )}
        {task.status === "to_do" && (
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => onTransition("backlog")}>
            <Archive className="size-3.5" />
            移入积压
          </Button>
        )}
        {task.status === "backlog" && (
          <Button size="sm" variant="secondary" disabled={busy} onClick={() => onTransition("to_do")}>
            <Play className="size-3.5" />
            开始
          </Button>
        )}
        {(task.status === "to_do" || task.status === "in_progress") && (
          <Button size="sm" variant="ghost" disabled={busy} onClick={onCancel}>
            <X className="size-3.5" />
            取消
          </Button>
        )}
        {task.status === "blocked" && (
          <Button size="sm" variant="secondary" disabled={busy} onClick={onRetry}>
            <RotateCcw className="size-3.5" />
            重试
          </Button>
        )}
        {isSettled && (
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => onTransition("to_do")}>
            <RotateCcw className="size-3.5" />
            重新打开
          </Button>
        )}
        {isSettled && (
          <button
            type="button"
            aria-label="移除任务"
            title="移除任务（会话会保留）"
            disabled={busy}
            onClick={onRemove}
            className={ICON_BUTTON}
          >
            <Trash2 className="size-4" />
          </button>
        )}
      </footer>
    </aside>
  );
}
