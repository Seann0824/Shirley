//! Task-pool scheduling for [`ProjectRuntimeManager`].
//!
//! The model lives in `crate::pool`; this file is the part that needs a running application: it
//! turns a queued task into a background session, waits for that session's `Finished` event, turns
//! the run's outcome into a [`Delivery`], and then starts the next queued task.
//!
//! Why a session per task instead of a new executor: sessions already own everything a job needs --
//! an `Agent` with its own message history, a working directory, a persisted log, a broadcast
//! channel, and the ability to run in the background while another session is in the foreground
//! (`spawn_turn` takes the `Agent` out, `tokio::spawn`s the turn, and puts it back). A second
//! executor would be a second copy of all of that.
//!
//! Consequences worth knowing:
//!
//! - The user's foreground selection is never touched: `create_pool_session` does not move the
//!   `active` pointer.
//! - A rework reuses the task's session, so the agent keeps the context of what it already built.
//! - Delivery files are collected by **time window** (`ArtifactStore::list` entries added after
//!   this attempt started). The store is append-only per project, so the window is the only honest
//!   way to attribute files to one run without threading a counter through the SDK.
//! - The frontend polls `pool_list` while the pool surface is open instead of us emitting a new
//!   event channel: the pool is a small, low-frequency list, and polling keeps the wire contract
//!   (already carrying `AgentEvent`) unchanged.

use std::sync::Arc;

use shirley_agent_sdk::{AgentEvent, MessageContent};
use tokio::sync::{Mutex, broadcast};

use super::ProjectRuntimeManager;
use crate::pool::{self, Delivery, DeliveryFile, PoolTask, TaskPool};

/// A task whose turn has not finished by now is treated as lost (a wedged provider, an endless
/// tool loop). It fails the task rather than holding an execution slot forever.
const TASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

impl ProjectRuntimeManager {
    /// The pool of `project`, opened on first use.
    pub async fn pool(&self, project: &str) -> Result<Arc<Mutex<TaskPool>>, String> {
        if let Some(handle) = self.pools.lock().await.get(project) {
            return Ok(Arc::clone(handle));
        }
        let runtime = self.ensure_runtime(project).await?;
        let path = {
            let guard = runtime.lock().await;
            pool_path(&guard.factory.working_dir)
        };
        let handle = Arc::new(Mutex::new(TaskPool::open(path)));
        self.pools
            .lock()
            .await
            .insert(project.to_owned(), Arc::clone(&handle));
        Ok(handle)
    }

    /// Tasks of a project, ordered for the review surface. Opening the list also nudges the queue:
    /// after a restart nothing else would ever start the tasks that were still waiting.
    pub async fn pool_list(self: &Arc<Self>, project: &str) -> Result<Vec<PoolTask>, String> {
        let handle = self.pool(project).await?;
        let tasks = handle.lock().await.ordered();
        if tasks
            .iter()
            .any(|task| task.status == pool::TaskStatus::ToDo)
        {
            let manager = Arc::clone(self);
            let project = project.to_owned();
            tokio::spawn(async move {
                if let Err(error) = manager.pool_pump(&project).await {
                    eprintln!("task pool pump failed for {project}: {error}");
                }
            });
        }
        Ok(tasks)
    }

    /// Drop a new instruction into the queue and start it if a slot is free.
    pub async fn pool_enqueue(
        self: &Arc<Self>,
        project: &str,
        brief: &str,
    ) -> Result<Vec<PoolTask>, String> {
        let handle = self.pool(project).await?;
        handle.lock().await.enqueue(project, brief)?;
        self.pool_pump(project).await?;
        Ok(handle.lock().await.ordered())
    }

    /// Accept a delivery: the task is finished for good.
    pub async fn pool_accept(&self, project: &str, id: &str) -> Result<Vec<PoolTask>, String> {
        let handle = self.pool(project).await?;
        let mut guard = handle.lock().await;
        guard.accept(id)?;
        Ok(guard.ordered())
    }

    /// Send a delivery back with a note; the task re-enters the queue and runs again in the same
    /// session.
    pub async fn pool_reject(
        self: &Arc<Self>,
        project: &str,
        id: &str,
        note: &str,
    ) -> Result<Vec<PoolTask>, String> {
        let handle = self.pool(project).await?;
        handle.lock().await.reject(id, note)?;
        self.pool_pump(project).await?;
        Ok(handle.lock().await.ordered())
    }

    /// Run a failed task again without changing its instruction.
    pub async fn pool_retry(
        self: &Arc<Self>,
        project: &str,
        id: &str,
    ) -> Result<Vec<PoolTask>, String> {
        let handle = self.pool(project).await?;
        handle.lock().await.retry(id)?;
        self.pool_pump(project).await?;
        Ok(handle.lock().await.ordered())
    }

    /// Move a task along the workflow -- Jira's "transition issue": park it, start it, reopen it.
    ///
    /// The model owns what is legal; the only thing added here is the side effect: a task that just
    /// became `To Do` re-runs the queue immediately (otherwise it would sit there until the next
    /// click), and one that was cancelled mid-flight also stops its turn.
    pub async fn pool_transition(
        self: &Arc<Self>,
        project: &str,
        id: &str,
        status: pool::TaskStatus,
    ) -> Result<Vec<PoolTask>, String> {
        let handle = self.pool(project).await?;
        let session = {
            let mut guard = handle.lock().await;
            let session = guard.find(id).and_then(|task| task.session.clone());
            guard.transition(id, status)?;
            session
        };
        match status {
            pool::TaskStatus::ToDo => self.pool_pump(project).await?,
            pool::TaskStatus::Cancelled => {
                if let Some(session) = session {
                    self.cancel(project, &session).await;
                }
            }
            _ => {}
        }
        Ok(handle.lock().await.ordered())
    }

    /// Withdraw a task. A running task also has its turn cancelled.
    pub async fn pool_cancel(&self, project: &str, id: &str) -> Result<Vec<PoolTask>, String> {
        let handle = self.pool(project).await?;
        let session = {
            let mut guard = handle.lock().await;
            let session = guard.find(id).and_then(|task| task.session.clone());
            guard.cancel(id)?;
            session
        };
        if let Some(session) = session {
            self.cancel(project, &session).await;
        }
        Ok(handle.lock().await.ordered())
    }

    /// Forget a task (the session it used is left alone).
    pub async fn pool_remove(&self, project: &str, id: &str) -> Result<Vec<PoolTask>, String> {
        let handle = self.pool(project).await?;
        let mut guard = handle.lock().await;
        guard.remove(id)?;
        Ok(guard.ordered())
    }

    /// Start queued tasks until every execution slot is busy.
    pub async fn pool_pump(self: &Arc<Self>, project: &str) -> Result<(), String> {
        loop {
            let handle = self.pool(project).await?;
            let task = {
                let guard = handle.lock().await;
                if guard.active() >= pool::MAX_CONCURRENT {
                    return Ok(());
                }
                match guard.next_queued() {
                    Some(task) => task,
                    None => return Ok(()),
                }
            };
            self.pool_start(project, task).await?;
        }
    }

    /// Take one task from `Queued` to `Running`: bind a session, subscribe **before** the turn
    /// starts, and hand the waiting to a background watcher.
    async fn pool_start(self: &Arc<Self>, project: &str, task: PoolTask) -> Result<(), String> {
        let runtime = self.ensure_runtime(project).await?;
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
        let handle = self.pool(project).await?;
        handle.lock().await.claim(&task.id, &session)?;

        // Subscribe before spawning: the turn publishes `Finished` into the session's broadcast, and
        // a receiver created afterwards would miss it.
        let receiver = {
            let guard = runtime.lock().await;
            guard.sessions.subscribe(&session)
        };
        let Some(receiver) = receiver else {
            handle
                .lock()
                .await
                .fail(&task.id, "the task session is unavailable")?;
            return Ok(());
        };

        let prompt = pool::task_prompt(&task.brief, task.note.as_deref());
        let window_start = pool::now_ms();
        if let Err(error) = self
            .spawn_turn(
                project,
                Some(session.clone()),
                task.brief.clone(),
                MessageContent::text(prompt),
            )
            .await
        {
            handle.lock().await.fail(&task.id, error)?;
            return Ok(());
        }

        spawn_watch(
            Arc::clone(self),
            project.to_owned(),
            task.id.clone(),
            window_start,
            receiver,
        );
        Ok(())
    }
}

/// Wait for one task turn to end and write the outcome back into the pool.
async fn pool_watch(
    manager: &Arc<ProjectRuntimeManager>,
    project: &str,
    id: &str,
    window_start: u64,
    mut receiver: broadcast::Receiver<Result<AgentEvent, String>>,
) {
    let outcome = tokio::time::timeout(TASK_TIMEOUT, wait_finished(&mut receiver)).await;
    let delivery = match outcome {
        Ok(Ok(result)) => match pool::deliverable_summary(&result.messages) {
            Some(summary) => Delivery {
                summary,
                files: artifact_files(manager, project, window_start).await,
                error: None,
            },
            None => Delivery::failed("the run ended without a delivery note"),
        },
        Ok(Err(error)) => Delivery::failed(error),
        Err(_) => Delivery::failed("the task timed out"),
    };
    let Ok(handle) = manager.pool(project).await else {
        return;
    };
    let mut guard = handle.lock().await;
    let failed = delivery.error.is_some();
    let written = if failed {
        guard.fail(id, delivery.error.clone().unwrap_or_default())
    } else {
        guard.complete(id, delivery)
    };
    if let Err(error) = written {
        eprintln!("task pool could not record {id}: {error}");
    }
    drop(guard);

    // Filling the freed slot from a fresh task keeps one task from serialising the whole queue.
    spawn_pump(Arc::clone(manager), project.to_owned());
}

/// Spawn the waiting side of one task.
///
/// Both spawns below live in **sync** helpers on purpose: `tokio::spawn` inside `pool_start` would
/// make the compiler infer `pool_start`'s opaque future while borrowing-checking a task that calls
/// back into `pool_pump`, which in turn needs `pool_start` — reported as either
/// "cannot satisfy `impl Future: Send`" or E0391 "cycle detected when computing type of opaque".
/// Outside the `async fn` bodies the recursive obligation is checked in a separate pass and the
/// cycle disappears.
fn spawn_watch(
    manager: Arc<ProjectRuntimeManager>,
    project: String,
    id: String,
    window_start: u64,
    receiver: broadcast::Receiver<Result<AgentEvent, String>>,
) {
    tokio::spawn(async move {
        pool_watch(&manager, &project, &id, window_start, receiver).await;
        spawn_pump(manager, project);
    });
}

/// Start queued tasks again once a slot is free.
fn spawn_pump(manager: Arc<ProjectRuntimeManager>, project: String) {
    tokio::spawn(async move {
        if let Err(error) = manager.pool_pump(&project).await {
            eprintln!("task pool pump failed for {project}: {error}");
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
///
/// The store is asked through the project runtime rather than from a captured `&AgentFactory`: a
/// reference to a `Send`-but-not-`Sync` factory would make the waiting task non-`Send`, and a
/// `MutexGuard` is not held across an await here.
async fn artifact_files(
    manager: &ProjectRuntimeManager,
    project: &str,
    since_ms: u64,
) -> Vec<DeliveryFile> {
    let Ok(runtime) = manager.ensure_runtime(project).await else {
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

/// `task-pool.json` inside the project's state directory (the same place sessions live).
fn pool_path(working_dir: &std::path::Path) -> std::path::PathBuf {
    crate::storage::StoragePaths::new(crate::storage::global_root(), working_dir)
        .map_or_else(|_| working_dir.join(".shirley"), |paths| paths.state)
        .join(pool::POOL_FILE_NAME)
}
