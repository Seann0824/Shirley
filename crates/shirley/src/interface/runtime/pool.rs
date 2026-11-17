//! Task-pool scheduling for [`BotRuntimeManager`].
//!
//! The model lives in `crate::pool`; this file is the part that needs a running application: it
//! turns a queued task into a background session of the task's bot, waits for that session's
//! `Finished` event, turns the run's outcome into a [`Delivery`], and then starts the next queued
//! task.
//!
//! There is **one pool** for the whole platform (`~/.shirley/tasks.json`); each task records the
//! bot that runs it. A session per task (rather than a second executor) is used on purpose:
//! sessions already own an `Agent` with its own history, a persisted log, a broadcast channel and
//! the ability to run in the background while the bot's main conversation is in the foreground.
//!
//! - A task's session is never the bot's `main` conversation, so the pool does not move the
//!   foreground.
//! - A rework reuses the task's session, so the agent keeps the context of what it already built.
//! - Delivery files are collected by **time window** (`ArtifactStore::list` entries added after
//!   this attempt started): the store is append-only, so the window is the only honest way to
//!   attribute files to one run.
//! - The frontend polls `pool_list` while the task surface is open; the pool is a small,
//!   low-frequency list.

use std::sync::Arc;

use shirley_agent_sdk::{AgentEvent, MessageContent};
use tokio::sync::broadcast;

use super::BotRuntimeManager;
use crate::pool::{self, Delivery, DeliveryFile, PoolTask};

/// A task whose turn has not finished by now is treated as lost (a wedged provider, an endless
/// tool loop). It fails the task rather than holding an execution slot forever.
const TASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// `tasks.json` under the application root.
pub(super) fn pool_path() -> std::path::PathBuf {
    crate::storage::global_root().join(pool::POOL_FILE_NAME)
}

impl BotRuntimeManager {
    /// Every task, ordered for the review surface. Opening the list also nudges the queue:
    /// after a restart nothing else would ever start the tasks that were still waiting.
    pub async fn pool_list(self: &Arc<Self>) -> Result<Vec<PoolTask>, String> {
        let tasks = self.pool.lock().await.ordered();
        if tasks
            .iter()
            .any(|task| task.status == pool::TaskStatus::ToDo)
        {
            spawn_pump(Arc::clone(self));
        }
        Ok(tasks)
    }

    /// Drop a new instruction for `bot` into the queue and start it if a slot is free.
    pub async fn pool_enqueue(
        self: &Arc<Self>,
        bot: &str,
        brief: &str,
    ) -> Result<Vec<PoolTask>, String> {
        self.pool.lock().await.enqueue(bot, brief)?;
        self.pool_pump().await?;
        Ok(self.pool.lock().await.ordered())
    }

    /// Accept a delivery: the task is finished for good.
    pub async fn pool_accept(&self, id: &str) -> Result<Vec<PoolTask>, String> {
        let mut guard = self.pool.lock().await;
        guard.accept(id)?;
        Ok(guard.ordered())
    }

    /// Send a delivery back with a note; the task re-enters the queue and runs again in the same
    /// session.
    pub async fn pool_reject(
        self: &Arc<Self>,
        id: &str,
        note: &str,
    ) -> Result<Vec<PoolTask>, String> {
        self.pool.lock().await.reject(id, note)?;
        self.pool_pump().await?;
        Ok(self.pool.lock().await.ordered())
    }

    /// Run a failed task again without changing its instruction.
    pub async fn pool_retry(self: &Arc<Self>, id: &str) -> Result<Vec<PoolTask>, String> {
        self.pool.lock().await.retry(id)?;
        self.pool_pump().await?;
        Ok(self.pool.lock().await.ordered())
    }

    /// Move a task along the workflow: park it, start it, reopen it.
    ///
    /// The model owns what is legal; the only thing added here is the side effect: a task that just
    /// became `To Do` re-runs the queue immediately, and one that was cancelled mid-flight also
    /// stops its turn.
    pub async fn pool_transition(
        self: &Arc<Self>,
        id: &str,
        status: pool::TaskStatus,
    ) -> Result<Vec<PoolTask>, String> {
        let running = {
            let mut guard = self.pool.lock().await;
            let running = guard
                .find(id)
                .and_then(|task| Some((task.bot.clone(), task.session.clone()?)));
            guard.transition(id, status)?;
            running
        };
        match status {
            pool::TaskStatus::ToDo => self.pool_pump().await?,
            pool::TaskStatus::Cancelled => {
                if let Some((bot, session)) = running {
                    self.cancel(&bot, &session).await;
                }
            }
            _ => {}
        }
        Ok(self.pool.lock().await.ordered())
    }

    /// Withdraw a task. A running task also has its turn cancelled.
    pub async fn pool_cancel(&self, id: &str) -> Result<Vec<PoolTask>, String> {
        let running = {
            let mut guard = self.pool.lock().await;
            let running = guard
                .find(id)
                .and_then(|task| Some((task.bot.clone(), task.session.clone()?)));
            guard.cancel(id)?;
            running
        };
        if let Some((bot, session)) = running {
            self.cancel(&bot, &session).await;
        }
        Ok(self.pool.lock().await.ordered())
    }

    /// Forget a task (the session it used is left alone).
    pub async fn pool_remove(&self, id: &str) -> Result<Vec<PoolTask>, String> {
        let mut guard = self.pool.lock().await;
        guard.remove(id)?;
        Ok(guard.ordered())
    }

    /// The resident scheduler's tick (`interface/server::run`): start whatever is queued.
    pub async fn pool_tick(self: &Arc<Self>) {
        if let Err(error) = self.pool_pump().await {
            eprintln!("[pool] tick failed: {error}");
        }
    }

    /// Start queued tasks until every execution slot is busy.
    pub async fn pool_pump(self: &Arc<Self>) -> Result<(), String> {
        loop {
            let task = {
                let guard = self.pool.lock().await;
                if guard.active() >= pool::MAX_CONCURRENT {
                    return Ok(());
                }
                match guard.next_queued() {
                    Some(task) => task,
                    None => return Ok(()),
                }
            };
            self.pool_start(task).await?;
        }
    }

    /// Take one task from `To Do` to `In Progress`: bind a session of the task's bot, subscribe
    /// **before** the turn starts, and hand the waiting to a background watcher.
    async fn pool_start(self: &Arc<Self>, task: PoolTask) -> Result<(), String> {
        let bot = task.bot.clone();
        let runtime = self.ensure_runtime(&bot).await?;
        // A task keeps one session for its whole life. A session recorded by a previous process is
        // no longer in memory, so a rework after a restart starts a fresh conversation rather than
        // reloading a log the user never asked to read.
        let session = {
            let mut guard = runtime.lock().await;
            match task
                .session
                .as_deref()
                .filter(|name| guard.sessions.session(name).is_some())
            {
                Some(name) => name.to_owned(),
                None => guard
                    .sessions
                    .create_pool_session(Some(&task.title))
                    .map_err(|error| format!("could not open a session for the task: {error}"))?,
            }
        };
        self.pool.lock().await.claim(&task.id, &session)?;

        // Subscribe before spawning: the turn publishes `Finished` into the session's broadcast, and
        // a receiver created afterwards would miss it.
        let receiver = {
            let guard = runtime.lock().await;
            guard.sessions.subscribe(&session)
        };
        let Some(receiver) = receiver else {
            self.pool
                .lock()
                .await
                .fail(&task.id, "the task session is unavailable")?;
            return Ok(());
        };

        let prompt = pool::task_prompt(&task.brief, task.note.as_deref());
        let window_start = pool::now_ms();
        if let Err(error) = self
            .spawn_turn(
                &bot,
                Some(session.clone()),
                task.brief.clone(),
                MessageContent::text(prompt),
            )
            .await
        {
            self.pool.lock().await.fail(&task.id, error)?;
            return Ok(());
        }

        spawn_watch(
            Arc::clone(self),
            bot,
            task.id.clone(),
            window_start,
            receiver,
        );
        Ok(())
    }
}

/// Wait for one task turn to end and write the outcome back into the pool.
async fn pool_watch(
    manager: &Arc<BotRuntimeManager>,
    bot: &str,
    id: &str,
    window_start: u64,
    mut receiver: broadcast::Receiver<Result<AgentEvent, String>>,
) {
    let outcome = tokio::time::timeout(TASK_TIMEOUT, wait_finished(&mut receiver)).await;
    let delivery = match outcome {
        Ok(Ok(result)) => match pool::deliverable_summary(&result.messages) {
            Some(summary) => Delivery {
                summary,
                files: artifact_files(manager, bot, window_start).await,
                error: None,
            },
            None => Delivery::failed("the run ended without a delivery note"),
        },
        Ok(Err(error)) => Delivery::failed(error),
        Err(_) => Delivery::failed("the task timed out"),
    };
    let mut guard = manager.pool.lock().await;
    let written = match delivery.error.clone() {
        Some(error) => guard.fail(id, error),
        None => guard.complete(id, delivery),
    };
    if let Err(error) = written {
        eprintln!("task pool could not record {id}: {error}");
    }
}

/// Spawn the waiting side of one task.
///
/// Both spawns below live in **sync** helpers on purpose: `tokio::spawn` inside `pool_start` would
/// make the compiler infer `pool_start`'s opaque future while borrow-checking a task that calls
/// back into `pool_pump`, which in turn needs `pool_start` -- reported as either
/// "cannot satisfy `impl Future: Send`" or E0391 "cycle detected when computing type of opaque".
/// Outside the `async fn` bodies the recursive obligation is checked in a separate pass.
fn spawn_watch(
    manager: Arc<BotRuntimeManager>,
    bot: String,
    id: String,
    window_start: u64,
    receiver: broadcast::Receiver<Result<AgentEvent, String>>,
) {
    tokio::spawn(async move {
        pool_watch(&manager, &bot, &id, window_start, receiver).await;
        // Filling the freed slot keeps one task from serialising the whole queue.
        spawn_pump(manager);
    });
}

/// Start queued tasks again once a slot is free.
fn spawn_pump(manager: Arc<BotRuntimeManager>) {
    tokio::spawn(async move {
        if let Err(error) = manager.pool_pump().await {
            eprintln!("task pool pump failed: {error}");
        }
    });
}

/// Consume a session's broadcast until the turn reports a result.
async fn wait_finished(
    receiver: &mut broadcast::Receiver<Result<AgentEvent, String>>,
) -> Result<shirley_agent_sdk::RunResult, String> {
    loop {
        match receiver.recv().await {
            Ok(Ok(AgentEvent::Finished(result))) => return Ok(result),
            Ok(Err(error)) => return Err(error),
            // Deltas and a lagged watcher are both noise: only the terminal event matters.
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => {
                return Err("the task session stopped unexpectedly".to_owned());
            }
        }
    }
}

/// Artifacts written during this attempt.
async fn artifact_files(
    manager: &BotRuntimeManager,
    bot: &str,
    since_ms: u64,
) -> Vec<DeliveryFile> {
    let Ok(runtime) = manager.ensure_runtime(bot).await else {
        return Vec::new();
    };
    let guard = runtime.lock().await;
    files_since(&guard.factory, since_ms)
}

/// Artifact-store entries added since `since_ms`, shaped for the pool.
fn files_since(factory: &crate::bootstrap::AgentFactory, since_ms: u64) -> Vec<DeliveryFile> {
    factory
        .artifacts
        .list()
        .into_iter()
        .filter(|entry| entry.added_ms >= since_ms)
        .map(|entry| DeliveryFile {
            id: entry.id,
            name: entry.name,
            mime: entry.mime.to_owned(),
            bytes: entry.bytes,
        })
        .collect()
}
