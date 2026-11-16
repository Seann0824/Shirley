//! Task pool: the user's "fire-and-forget" queue of jobs.
//!
//! The product idea is a different attention contract from a chat window. In a chat the human
//! watches the *process* and steers it. Here the human only ever sees two things: what came out,
//! and whether it is acceptable. So a task is a unit of *delivery*, not a conversation:
//!
//! - The user drops in an instruction and immediately drops in the next one. Nothing blocks on a
//!   running task -- the queue is the buffer.
//! - Each task runs in its own background session (`interface/runtime/pool.rs` drives
//!   `spawn_turn`), so a task keeps its working context across reworks.
//! - When a run ends, the pool collects a **deliverable** (the note the model wrote plus the files
//!   produced during the run) and parks the task in [`TaskStatus::InReview`]. Nothing is "done" until
//!   the user accepts it; a rejection goes back into the queue with the user's note as the new
//!   requirement, reusing the same session so the agent does not lose what it built.
//!
//! This module owns the **model and its persistence** only. Driving sessions, waiting for
//! `Finished` and collecting artifacts live in `interface/runtime/pool.rs`; the split keeps the
//! state machine testable without an `Agent`.
//!
//! Storage is one JSON file per bot (`task-pool.json` in the bot's state dir), rewritten
//! atomically on every transition: a pool that survives a crash is worth more than one that is
//! clever about batching writes.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use shirley_agent_sdk::Message;
use uuid::Uuid;

use crate::prompt_template;

/// File name of the pool inside a bot's state directory.
pub const POOL_FILE_NAME: &str = "task-pool.json";

/// How many tasks may execute at the same time in one bot.
///
/// A pool that runs everything at once is not a pool, it is a stampede: each task is a full agent
/// turn, and the user's only remaining job is judging output. Two keeps the queue visibly moving
/// without the provider's rate limits deciding the schedule for us.
pub const MAX_CONCURRENT: usize = 2;

/// Longest title kept from the instruction's first line.
const MAX_TITLE_CHARS: usize = 64;

const TASK_TEMPLATE: &str = include_str!("../../prompts/task_pool.md");
const REWORK_TEMPLATE: &str = include_str!("../../prompts/task_pool_rework.md");

/// Where a task sits in the workflow.
///
/// The names are Jira's on purpose: the user already knows what "To Do" and "In Review" mean, so
/// the board needs no explaining. Two states exist that a plain queue would not have:
///
/// - [`Backlog`](Self::Backlog) -- dropped in but deliberately not scheduled. Without it the only
///   way to slow down is to not write the instruction at all.
/// - [`Blocked`](Self::Blocked) -- the run broke. Not a terminal "failed": a human still has to
///   decide, so it sits with the things that need the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Paused by the user; never picked up by the scheduler.
    Backlog,
    /// Waiting for a slot; the oldest one starts next.
    #[serde(alias = "queued", alias = "todo")]
    ToDo,
    /// A turn is running in the background session.
    #[serde(alias = "running")]
    InProgress,
    /// The run ended; the deliverable is waiting for the user's verdict.
    #[serde(alias = "review")]
    InReview,
    /// The run broke (model or tool error, timeout); [`Delivery::error`] says why.
    #[serde(alias = "failed")]
    Blocked,
    /// The user accepted the deliverable.
    #[serde(alias = "accepted")]
    Done,
    /// The user withdrew the task.
    Cancelled,
}

impl TaskStatus {
    /// Whether nothing will move this task on its own any more.
    #[must_use]
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    /// The name the user sees. Kept next to the variant so the workflow reads in one place.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::ToDo => "To Do",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::Blocked => "Blocked",
            Self::Done => "Done",
            Self::Cancelled => "Cancelled",
        }
    }

    /// Sort rank: what needs the user first, what is in flight next, what is closed last.
    const fn rank(self) -> u8 {
        match self {
            Self::InReview => 0,
            Self::Blocked => 1,
            Self::InProgress => 2,
            Self::ToDo => 3,
            Self::Backlog => 4,
            Self::Done => 5,
            Self::Cancelled => 6,
        }
    }

    /// The workflow, written down once: every move a caller may ask for, and nothing else.
    ///
    /// A rejection here is the point -- a stale board must not be able to resurrect a closed task
    /// or skip review by editing the file by hand.
    #[must_use]
    pub fn can_move_to(self, to: Self) -> bool {
        match (self, to) {
            // Pause and resume. Anything that goes back to To Do from a further state is a
            // rework, which is a different (and billed) attempt.
            (Self::Backlog, Self::ToDo | Self::Cancelled)
            | (Self::ToDo, Self::Backlog | Self::InProgress | Self::Cancelled)
            | (Self::InProgress, Self::InReview | Self::Blocked | Self::Cancelled)
            | (Self::InReview, Self::Done | Self::ToDo | Self::Cancelled)
            | (Self::Blocked, Self::ToDo | Self::Backlog | Self::Cancelled)
            // Reopen, so a wrong "Done" is not permanent.
            | (Self::Done | Self::Cancelled, Self::ToDo) => true,
            _ => false,
        }
    }
}

/// A file produced during a run, by reference (the bytes live in the artifact store).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryFile {
    /// Artifact id -- the same handle the library and the `media` tool use.
    pub id: String,
    pub name: String,
    pub mime: String,
    pub bytes: u64,
}

/// What the user is asked to judge.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    /// The model's final message: what it delivered and how to check it.
    pub summary: String,
    /// Files that appeared in the artifact store while this attempt ran.
    #[serde(default)]
    pub files: Vec<DeliveryFile>,
    /// Failure text when the attempt produced no delivery (`None` on success).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Delivery {
    /// A failed attempt: no summary, no files, the reason kept for display.
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            summary: String::new(),
            files: Vec::new(),
            error: Some(reason.into()),
        }
    }
}

/// One task in the pool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolTask {
    pub id: String,
    /// Bot (workspace) this task belongs to; one bot's pool never lists another's.
    pub bot: String,
    /// Short label derived from the instruction's first line.
    pub title: String,
    /// The user's instruction, verbatim. Also the definition of done.
    pub brief: String,
    pub status: TaskStatus,
    /// 1 on the first run, incremented on every rework.
    pub attempt: u32,
    pub created_ms: u64,
    #[serde(default)]
    pub started_ms: Option<u64>,
    #[serde(default)]
    pub finished_ms: Option<u64>,
    /// Background session backing this task (reused across reworks, so context survives).
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub delivery: Option<Delivery>,
    /// The user's rejection note -- becomes the requirement for the next attempt.
    #[serde(default)]
    pub note: Option<String>,
}

/// The pool of one bot, plus the file it is stored in.
#[derive(Debug)]
pub struct TaskPool {
    path: PathBuf,
    tasks: Vec<PoolTask>,
}

impl TaskPool {
    /// Open (or start) the pool stored at `path`.
    ///
    /// A task left in `Running` belongs to a process that died -- it is pushed back to the queue
    /// instead of being shown as "running forever".
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut tasks = load(&path);
        let mut interrupted = false;
        for task in &mut tasks {
            if task.status == TaskStatus::InProgress {
                task.status = TaskStatus::ToDo;
                task.started_ms = None;
                interrupted = true;
            }
        }
        let pool = Self { path, tasks };
        if interrupted {
            let _ = pool.save();
        }
        pool
    }

    /// Where the pool is persisted -- only the tests reopen it from disk.
    #[cfg(test)]
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// View order: needs-review first, then in flight, then settled; newest first in a group
    /// except for the queue itself, which is FIFO so "next up" reads naturally.
    #[must_use]
    pub fn ordered(&self) -> Vec<PoolTask> {
        let mut tasks = self.tasks.clone();
        tasks.sort_by(|left, right| {
            left.status
                .rank()
                .cmp(&right.status.rank())
                .then_with(|| match left.status {
                    // The queue reads top-down as "next up", so it stays FIFO.
                    TaskStatus::ToDo => left.created_ms.cmp(&right.created_ms),
                    _ => right.created_ms.cmp(&left.created_ms),
                })
        });
        tasks
    }

    /// Number of tasks currently holding an execution slot.
    #[must_use]
    pub fn active(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| task.status == TaskStatus::InProgress)
            .count()
    }

    /// The oldest queued task -- the one that starts next.
    #[must_use]
    pub fn next_queued(&self) -> Option<PoolTask> {
        self.tasks
            .iter()
            .filter(|task| task.status == TaskStatus::ToDo)
            .min_by_key(|task| task.created_ms)
            .cloned()
    }

    /// Look one task up.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&PoolTask> {
        self.tasks.iter().find(|task| task.id == id)
    }

    /// Add an instruction to the end of the queue.
    pub fn enqueue(&mut self, bot: &str, brief: &str) -> Result<PoolTask, String> {
        let brief = brief.trim();
        if brief.is_empty() {
            return Err("a task needs an instruction".to_owned());
        }
        let task = PoolTask {
            id: Uuid::new_v4().to_string(),
            bot: bot.to_owned(),
            title: derive_title(brief),
            brief: brief.to_owned(),
            // New work runs: the whole promise of the pool is that dropping an instruction in
            // does not also demand that you go press "start".
            status: TaskStatus::ToDo,
            attempt: 1,
            created_ms: now_ms(),
            started_ms: None,
            finished_ms: None,
            session: None,
            delivery: None,
            note: None,
        };
        self.tasks.push(task.clone());
        self.save()?;
        Ok(task)
    }

    /// Move a task along the workflow -- the single place a status may change.
    ///
    /// Every move is checked against [`TaskStatus::can_move_to`], so a stale board cannot
    /// resurrect a closed task or skip review. Going back to `To Do` from anywhere but `Backlog`
    /// counts as a new attempt: memory kept, work redone.
    pub fn transition(&mut self, id: &str, to: TaskStatus) -> Result<(), String> {
        let task = self.task_mut(id)?;
        let from = task.status;
        if from == to {
            return Ok(());
        }
        if !from.can_move_to(to) {
            return Err(format!(
                "a task in {} cannot move to {}",
                from.label(),
                to.label()
            ));
        }
        task.status = to;
        match to {
            TaskStatus::InProgress => {
                task.started_ms = Some(now_ms());
                task.finished_ms = None;
            }
            TaskStatus::ToDo => {
                // Backlog -> To Do is a first start; anything else is a rework, and a rework is
                // worth counting: it is the number that says the brief was unclear.
                if from != TaskStatus::Backlog {
                    task.attempt += 1;
                }
                task.started_ms = None;
                task.finished_ms = None;
            }
            TaskStatus::Done | TaskStatus::Cancelled => task.finished_ms = Some(now_ms()),
            _ => task.finished_ms = None,
        }
        self.save()
    }

    /// Take a to-do task into `In Progress`, recording the session that executes it.
    pub fn claim(&mut self, id: &str, session: &str) -> Result<(), String> {
        let task = self.task_mut(id)?;
        if !task.status.can_move_to(TaskStatus::InProgress) {
            return Err(format!(
                "only a to-do task can start, this one is {}",
                task.status.label()
            ));
        }
        task.status = TaskStatus::InProgress;
        task.started_ms = Some(now_ms());
        task.finished_ms = None;
        task.session = Some(session.to_owned());
        self.save()
    }

    /// Park a finished run in `In Review` with its deliverable.
    pub fn complete(&mut self, id: &str, delivery: Delivery) -> Result<(), String> {
        let task = self.task_mut(id)?;
        // The user withdrew the task while the turn was in flight: their verdict wins.
        if task.status == TaskStatus::Cancelled {
            return Ok(());
        }
        task.status = TaskStatus::InReview;
        task.finished_ms = Some(now_ms());
        task.delivery = Some(delivery);
        self.save()
    }

    /// Mark a broken attempt as `Blocked` (model error, transport error, panic, timeout).
    pub fn fail(&mut self, id: &str, reason: impl Into<String>) -> Result<(), String> {
        let task = self.task_mut(id)?;
        if task.status == TaskStatus::Cancelled {
            return Ok(());
        }
        task.status = TaskStatus::Blocked;
        task.finished_ms = Some(now_ms());
        task.delivery = Some(Delivery::failed(reason));
        self.save()
    }

    /// Accept a delivery: the task is done for good.
    pub fn accept(&mut self, id: &str) -> Result<(), String> {
        let task = self.task_mut(id)?;
        if !task.status.can_move_to(TaskStatus::Done) {
            return Err(format!(
                "only a task in review can be accepted, this one is {}",
                task.status.label()
            ));
        }
        task.status = TaskStatus::Done;
        task.finished_ms.get_or_insert_with(now_ms);
        self.save()
    }

    /// Send a delivery back: the task returns to `To Do` with `note` as the new requirement.
    ///
    /// The session is deliberately kept -- the next attempt continues in the same conversation, so
    /// the agent does not rebuild what it already built. The previous delivery is kept too, so the
    /// card can still show what was rejected while the rework runs.
    pub fn reject(&mut self, id: &str, note: &str) -> Result<(), String> {
        let note = note.trim();
        if note.is_empty() {
            return Err("say what needs to change before sending a task back".to_owned());
        }
        let task = self.task_mut(id)?;
        if !task.status.can_move_to(TaskStatus::ToDo) {
            return Err(format!(
                "only a task in review can be sent back, this one is {}",
                task.status.label()
            ));
        }
        task.status = TaskStatus::ToDo;
        task.attempt += 1;
        task.note = Some(note.to_owned());
        task.started_ms = None;
        task.finished_ms = None;
        self.save()
    }

    /// Requeue a blocked (or cancelled) task, keeping its instruction and note.
    pub fn retry(&mut self, id: &str) -> Result<(), String> {
        let status = self
            .find(id)
            .map(|task| task.status)
            .ok_or_else(|| "no such task".to_owned())?;
        if !matches!(status, TaskStatus::Blocked | TaskStatus::Cancelled) {
            return Err("only a blocked or cancelled task can be retried".to_owned());
        }
        self.transition(id, TaskStatus::ToDo)
    }

    /// Withdraw a task that is still open.
    pub fn cancel(&mut self, id: &str) -> Result<(), String> {
        let task = self.task_mut(id)?;
        if task.status.is_settled() {
            return Err("this task is already closed".to_owned());
        }
        task.status = TaskStatus::Cancelled;
        task.finished_ms = Some(now_ms());
        self.save()
    }

    /// Drop a task from the pool entirely (the session it used stays where it is).
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let before = self.tasks.len();
        self.tasks.retain(|task| task.id != id);
        if self.tasks.len() == before {
            return Err("no such task".to_owned());
        }
        self.save()
    }

    fn task_mut(&mut self, id: &str) -> Result<&mut PoolTask, String> {
        self.tasks
            .iter_mut()
            .find(|task| task.id == id)
            .ok_or_else(|| "no such task".to_owned())
    }

    /// Rewrite the file atomically (temp file + rename), so a crash never truncates the queue.
    pub fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create the task pool dir: {error}"))?;
        }
        let body = serde_json::to_string_pretty(&self.tasks)
            .map_err(|error| format!("could not encode the task pool: {error}"))?;
        let temp = self.path.with_extension("json.tmp");
        fs::write(&temp, body)
            .map_err(|error| format!("could not write the task pool: {error}"))?;
        fs::rename(&temp, &self.path)
            .map_err(|error| format!("could not replace the task pool: {error}"))
    }
}

fn load(path: &Path) -> Vec<PoolTask> {
    let Ok(body) = fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str(&body) {
        Ok(tasks) => tasks,
        Err(error) => {
            eprintln!(
                "task pool {} is unreadable, starting empty: {error}",
                path.display()
            );
            Vec::new()
        }
    }
}

/// Wall-clock milliseconds; the scheduler also stamps its artifact window with this.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// First non-empty line of the instruction, clipped -- a label, not a summary.
fn derive_title(brief: &str) -> String {
    let first = brief
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Untitled task");
    let chars: Vec<char> = first.chars().collect();
    if chars.len() <= MAX_TITLE_CHARS {
        return first.to_owned();
    }
    let mut title: String = chars.into_iter().take(MAX_TITLE_CHARS).collect();
    title.push('…');
    title
}

/// The instruction handed to the background session.
///
/// A rework appends the user's note: the original brief stays in place (the agent must still know
/// what the task is), and the note is stated to win on conflict -- which is exactly the review
/// semantics, expressed where the model will read it.
#[must_use]
pub fn task_prompt(brief: &str, note: Option<&str>) -> String {
    let mut prompt = prompt_template::render(TASK_TEMPLATE, &[("brief", brief.trim())]);
    let note = note.map(str::trim).filter(|note| !note.is_empty());
    if let Some(note) = note {
        prompt.push_str("\n\n");
        prompt.push_str(&prompt_template::render(REWORK_TEMPLATE, &[("note", note)]));
    }
    prompt
}

/// The text the user reads: the last assistant message of the turn.
///
/// `RunResult::messages` holds only what this turn added, so the last assistant message with
/// content is the delivery note. Earlier assistant messages with empty content are tool-call
/// carriers, not prose.
#[must_use]
pub fn deliverable_summary(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|message| match message {
        Message::Assistant {
            content: Some(content),
            ..
        } if !content.trim().is_empty() => Some(content.trim().to_owned()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shirley_agent_sdk::MessageContent;

    fn temp_pool(name: &str) -> TaskPool {
        let dir = std::env::temp_dir().join(format!("shirley-pool-{name}-{}", Uuid::new_v4()));
        TaskPool::open(dir.join(POOL_FILE_NAME))
    }

    #[test]
    fn enqueue_derives_title_and_persists() {
        let mut pool = temp_pool("enqueue");
        let task = pool
            .enqueue(
                "proj",
                "\n\n  Migrate the TS backend to Rust  \nsecond line",
            )
            .expect("enqueue");
        assert_eq!(task.title, "Migrate the TS backend to Rust");
        assert_eq!(task.status, TaskStatus::ToDo);
        assert_eq!(task.attempt, 1);

        let reopened = TaskPool::open(pool.path());
        assert_eq!(reopened.ordered().len(), 1);
        assert_eq!(reopened.ordered()[0].brief, task.brief);
    }

    #[test]
    fn empty_instruction_is_rejected() {
        let mut pool = temp_pool("empty");
        assert!(pool.enqueue("proj", "   \n ").is_err());
        assert!(pool.ordered().is_empty());
    }

    #[test]
    fn review_comes_before_running_and_queue() {
        let mut pool = temp_pool("order");
        let first = pool.enqueue("proj", "first").expect("enqueue");
        let second = pool.enqueue("proj", "second").expect("enqueue");
        pool.claim(&second.id, "s2").expect("claim");
        pool.complete(&second.id, Delivery::default())
            .expect("complete");
        let ids: Vec<String> = pool.ordered().into_iter().map(|task| task.id).collect();
        assert_eq!(ids, vec![second.id.clone(), first.id.clone()]);

        // Queue order is FIFO: the oldest instruction starts next.
        assert_eq!(pool.next_queued().map(|task| task.id), Some(first.id));
    }

    #[test]
    fn reject_requeues_with_the_note_and_keeps_the_session() {
        let mut pool = temp_pool("reject");
        let task = pool.enqueue("proj", "build a report").expect("enqueue");
        pool.claim(&task.id, "session-1").expect("claim");
        pool.complete(&task.id, Delivery::default())
            .expect("complete");
        pool.reject(&task.id, "  needs a CSV export too ")
            .expect("reject");

        let after = pool.find(&task.id).expect("task");
        assert_eq!(after.status, TaskStatus::ToDo);
        assert_eq!(after.attempt, 2);
        assert_eq!(after.attempt, 2);
        assert_eq!(after.note.as_deref(), Some("needs a CSV export too"));
        assert_eq!(after.session.as_deref(), Some("session-1"));
        // The rejected delivery is still visible while the rework runs.
        assert!(after.delivery.is_some());
        assert!(pool.reject(&task.id, "  ").is_err());
    }

    #[test]
    fn running_tasks_are_requeued_after_a_restart() {
        let mut pool = temp_pool("restart");
        let task = pool.enqueue("proj", "long job").expect("enqueue");
        pool.claim(&task.id, "session-1").expect("claim");
        assert_eq!(pool.active(), 1);

        let reopened = TaskPool::open(pool.path());
        let after = reopened.find(&task.id).expect("task");
        assert_eq!(after.status, TaskStatus::ToDo);
        assert_eq!(reopened.active(), 0);
        assert!(after.started_ms.is_none());
    }

    #[test]
    fn legacy_status_names_still_load() {
        // A pool written before the workflow was renamed to Jira's vocabulary must not be lost.
        let dir = std::env::temp_dir().join(format!("shirley-pool-legacy-{}", Uuid::new_v4()));
        let path = dir.join(POOL_FILE_NAME);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            &path,
            r#"[
              {"id":"a","bot":"p","title":"t","brief":"b","status":"queued","attempt":1,"createdMs":1},
              {"id":"b","bot":"p","title":"t","brief":"b","status":"accepted","attempt":1,"createdMs":2},
              {"id":"c","bot":"p","title":"t","brief":"b","status":"failed","attempt":1,"createdMs":3}
            ]"#,
        )
        .expect("write");
        let pool = TaskPool::open(&path);
        assert_eq!(pool.find("a").expect("a").status, TaskStatus::ToDo);
        assert_eq!(pool.find("b").expect("b").status, TaskStatus::Done);
        assert_eq!(pool.find("c").expect("c").status, TaskStatus::Blocked);
    }

    #[test]
    fn illegal_moves_are_rejected() {
        let mut pool = temp_pool("moves");
        let task = pool.enqueue("proj", "ship it").expect("enqueue");
        // Skip review: not a move the workflow allows.
        pool.claim(&task.id, "s1").expect("claim");
        pool.complete(&task.id, Delivery::default())
            .expect("complete");
        assert!(pool.transition(&task.id, TaskStatus::InProgress).is_err());
        pool.accept(&task.id).expect("accept");
        // Closed is closed until someone reopens it.
        assert!(pool.cancel(&task.id).is_err());
        assert!(pool.claim(&task.id, "s2").is_err());
        assert!(pool.transition(&task.id, TaskStatus::Cancelled).is_err());
    }

    #[test]
    fn backlog_pauses_without_counting_an_attempt() {
        let mut pool = temp_pool("backlog");
        let parked = pool.enqueue("proj", "later job").expect("enqueue");
        let running = pool.enqueue("proj", "now job").expect("enqueue");
        pool.transition(&parked.id, TaskStatus::Backlog)
            .expect("hold");

        // Parked work is not in the queue, so it cannot be picked up by accident.
        assert_eq!(
            pool.next_queued().map(|task| task.id),
            Some(running.id.clone())
        );
        // ... and it sorts below the queue, because it needs nothing from the user.
        let ids: Vec<String> = pool.ordered().into_iter().map(|task| task.id).collect();
        assert_eq!(ids, vec![running.id.clone(), parked.id.clone()]);

        pool.transition(&parked.id, TaskStatus::ToDo)
            .expect("start");
        let after = pool.find(&parked.id).expect("task");
        assert_eq!(after.status, TaskStatus::ToDo);
        // A first start is not a rework, so the attempt counter stays at 1.
        assert_eq!(after.attempt, 1);
    }

    #[test]
    fn block_is_not_terminal() {
        let mut pool = temp_pool("blocked");
        let task = pool.enqueue("proj", "flaky job").expect("enqueue");
        pool.claim(&task.id, "s1").expect("claim");
        pool.fail(&task.id, "provider said no").expect("fail");
        let after = pool.find(&task.id).expect("task");
        assert_eq!(after.status, TaskStatus::Blocked);
        assert!(!after.status.is_settled());
        assert_eq!(
            after.delivery.as_ref().and_then(|d| d.error.as_deref()),
            Some("provider said no")
        );

        pool.retry(&task.id).expect("retry");
        assert_eq!(pool.find(&task.id).expect("task").status, TaskStatus::ToDo);
        assert_eq!(pool.find(&task.id).expect("task").attempt, 2);
    }

    #[test]
    fn cancel_and_remove() {
        let mut pool = temp_pool("cancel");
        let task = pool.enqueue("proj", "drop me").expect("enqueue");
        pool.cancel(&task.id).expect("cancel");
        assert_eq!(
            pool.find(&task.id).expect("task").status,
            TaskStatus::Cancelled
        );
        assert!(pool.cancel(&task.id).is_err());
        pool.remove(&task.id).expect("remove");
        assert!(pool.find(&task.id).is_none());
        assert!(pool.remove(&task.id).is_err());
    }

    #[test]
    fn prompt_carries_brief_and_note() {
        let fresh = task_prompt("ship the parser", None);
        assert!(fresh.contains("ship the parser"));
        assert!(!fresh.contains("Rework"));

        let rework = task_prompt("ship the parser", Some("handle UTF-8"));
        assert!(rework.contains("ship the parser"));
        assert!(rework.contains("handle UTF-8"));
    }

    #[test]
    fn summary_is_the_last_assistant_text() {
        let messages = vec![
            Message::User {
                content: MessageContent::text("go"),
            },
            Message::Assistant {
                content: Some("draft".to_owned()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            Message::Assistant {
                content: Some("  Delivered: report.md  ".to_owned()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
        ];
        assert_eq!(
            deliverable_summary(&messages).as_deref(),
            Some("Delivered: report.md")
        );
    }
}
