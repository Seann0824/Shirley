//! A bot's handle on the task pool: dispatch work, add to it, and know what is on its plate.
//!
//! The pool is shared by the whole platform; a desk is that pool seen from one bot's main
//! conversation. Writing wakes the scheduler, so a dispatched task starts at once rather than on
//! the next timer tick.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{Mutex, mpsc};

use shirley_agent_sdk::ContextProvider;

use super::{PoolTask, TaskPool, TaskStatus, now_ms};

/// How much of a delivery note the bot is reminded of each turn.
const SUMMARY_CHARS: usize = 400;
/// Settled tasks older than this drop out of the reminder (they are still on the task page).
const RECENT_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_LISTED: usize = 8;

#[derive(Clone)]
pub struct TaskDesk {
    pool: Arc<Mutex<TaskPool>>,
    wake: mpsc::UnboundedSender<()>,
}

impl std::fmt::Debug for TaskDesk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskDesk").finish_non_exhaustive()
    }
}

impl TaskDesk {
    pub fn new(pool: Arc<Mutex<TaskPool>>, wake: mpsc::UnboundedSender<()>) -> Self {
        Self { pool, wake }
    }

    /// Queue work for `bot` and start it if a slot is free.
    pub async fn dispatch(&self, bot: &str, title: &str, brief: &str) -> Result<PoolTask, String> {
        let task = self
            .pool
            .lock()
            .await
            .enqueue_titled(bot, Some(title), brief)?;
        let _ = self.wake.send(());
        Ok(task)
    }

    /// Add to one of `bot`'s tasks (see [`TaskPool::add_note`]).
    pub async fn note(&self, bot: &str, id: &str, note: &str) -> Result<PoolTask, String> {
        let mut pool = self.pool.lock().await;
        match pool.find(id) {
            Some(task) if task.bot == bot => {}
            Some(_) => return Err("that task belongs to another bot".to_owned()),
            None => return Err(format!("no task with id {id}")),
        }
        pool.add_note(id, note)?;
        let task = pool.find(id).cloned().ok_or("no such task")?;
        drop(pool);
        let _ = self.wake.send(());
        Ok(task)
    }

    /// `bot`'s open tasks and the recently settled ones, newest first. `None` while the pool is
    /// being written (the reminder then reuses the previous text).
    fn snapshot(&self, bot: &str) -> Option<Vec<PoolTask>> {
        let pool = self.pool.try_lock().ok()?;
        let now = now_ms();
        let mut tasks: Vec<PoolTask> = pool
            .ordered()
            .into_iter()
            .filter(|task| task.bot == bot)
            .filter(|task| {
                !task.status.is_settled()
                    || task
                        .finished_ms
                        .is_some_and(|at| now.saturating_sub(at) < RECENT_MS)
            })
            .filter(|task| task.status != TaskStatus::Cancelled)
            .collect();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.created_ms));
        tasks.truncate(MAX_LISTED);
        Some(tasks)
    }
}

/// Reminds the bot, at the end of every request, what it has dispatched and how each went.
pub struct TaskContextProvider {
    desk: TaskDesk,
    bot: String,
    last: StdMutex<Option<String>>,
}

impl TaskContextProvider {
    pub fn new(desk: TaskDesk, bot: impl Into<String>) -> Self {
        Self {
            desk,
            bot: bot.into(),
            last: StdMutex::new(None),
        }
    }
}

impl ContextProvider for TaskContextProvider {
    fn context(&self) -> Option<String> {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(tasks) = self.desk.snapshot(&self.bot) {
            *last = render(&tasks);
        }
        last.clone()
    }
}

fn status_text(task: &PoolTask) -> &'static str {
    match task.status {
        TaskStatus::Backlog => "parked",
        TaskStatus::ToDo => "queued",
        TaskStatus::InProgress => "running",
        TaskStatus::InReview => "delivered, waiting for the user's verdict",
        TaskStatus::Blocked => "failed",
        TaskStatus::Done => "accepted by the user",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn render(tasks: &[PoolTask]) -> Option<String> {
    if tasks.is_empty() {
        return None;
    }
    let mut out = String::from(
        "<tasks>\nWork you dispatched with `dispatch_task`. The user sees each one as a card in this conversation and judges it there. Add to one with `task_note`.\n",
    );
    for task in tasks {
        let _ = writeln!(
            out,
            "- [{}] {} -- {}",
            task.id,
            task.title,
            status_text(task)
        );
        if !task.pending_notes.is_empty() {
            let _ = writeln!(
                out,
                "  waiting to be applied: {}",
                task.pending_notes.join(" / ")
            );
        }
        if let Some(delivery) = &task.delivery {
            let text = delivery.error.as_deref().unwrap_or(&delivery.summary);
            let clipped: String = text.chars().take(SUMMARY_CHARS).collect();
            let more = if text.chars().count() > SUMMARY_CHARS {
                " …"
            } else {
                ""
            };
            let _ = writeln!(out, "  result: {}{more}", clipped.replace('\n', " "));
        }
    }
    out.push_str("</tasks>");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::Delivery;

    fn desk(tag: &str) -> (TaskDesk, mpsc::UnboundedReceiver<()>) {
        let dir = std::env::temp_dir().join(format!("shirley-desk-{tag}-{}", uuid::Uuid::new_v4()));
        let pool = Arc::new(Mutex::new(TaskPool::open(dir.join("tasks.json"))));
        let (tx, rx) = mpsc::unbounded_channel();
        (TaskDesk::new(pool, tx), rx)
    }

    #[tokio::test]
    async fn dispatch_wakes_the_scheduler_and_shows_in_the_reminder() {
        let (desk, mut wake) = desk("dispatch");
        let task = desk
            .dispatch("roam", "云南路线图", "画一张自驾路线图")
            .await
            .unwrap();
        assert!(wake.try_recv().is_ok());
        let provider = TaskContextProvider::new(desk.clone(), "roam");
        let text = provider.context().expect("reminder");
        assert!(
            text.contains(&task.id) && text.contains("云南路线图") && text.contains("queued"),
            "{text}"
        );
        // Another bot's conversation does not see it.
        assert!(TaskContextProvider::new(desk, "patch").context().is_none());
    }

    #[tokio::test]
    async fn the_reminder_carries_the_delivery_and_notes_check_ownership() {
        let (desk, _wake) = desk("note");
        let task = desk.dispatch("roam", "路线图", "画图").await.unwrap();
        {
            let mut pool = desk.pool.lock().await;
            pool.claim(&task.id, "s").unwrap();
            pool.complete(
                &task.id,
                Delivery {
                    summary: "已交付路线图.html".into(),
                    ..Delivery::default()
                },
            )
            .unwrap();
        }
        let text = TaskContextProvider::new(desk.clone(), "roam")
            .context()
            .unwrap();
        assert!(text.contains("已交付路线图.html"), "{text}");
        assert!(desk.note("patch", &task.id, "改一下").await.is_err());
        let task = desk.note("roam", &task.id, "加预算").await.unwrap();
        assert_eq!(task.status, TaskStatus::ToDo);
    }
}
