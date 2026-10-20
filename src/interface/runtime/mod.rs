//! Shared multi-project runtime core.
//!
//! This module is the **interface-agnostic orchestration layer** that both transports
//! (the Tauri shell `interface/desktop/shell.rs` and the HTTP/SSE server
//! `interface/server.rs`) are thin wrappers over. It owns:
//!
//! - the [`ProjectRegistry`] (which directories are projects),
//! - the lazily-loaded per-project [`ProjectRuntime`] map (each = one [`AgentFactory`]
//!   + one [`SessionManager`]),
//! - the per-`project::session` cancel handles,
//! - the **turn driver** ([`ProjectRuntimeManager::spawn_turn`]) that takes a session's
//!   `Agent`, streams `run_stream`, feeds every `AgentEvent` back into the session and
//!   fans it out through a transport-supplied `sink`,
//! - all the query commands (projects / sessions / model / files / skills / subagents)
//!   and the wire DTOs they return.
//!
//! The core never holds a socket or a Tauri handle. Events leave it through a
//! `sink: Arc<dyn Fn(AgentEventWire)>`: the desktop passes a **no-op** sink (its events
//! flow through the per-session `Session::publish` channel, drained by a Tauri pump),
//! while the server passes a sink that serialises to its global SSE broadcast.
//!
//! See `docs/cloud-server.md` for the design rationale.

#![cfg_attr(
    not(any(feature = "desktop", feature = "server")),
    allow(
        dead_code,
        reason = "only used when the relevant interface feature is enabled"
    )
)]

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use serde::Serialize;
use shirley_agent_sdk::{
    Agent, AgentEvent, Content, ContentPart, ImagePart, Message, RunResult, StopReason, Usage,
};
use tokio::sync::{Mutex, broadcast, oneshot};

use super::desktop::wire::{AgentEventWire, SessionSnapshotWire, StateSnapshotWire, TaskViewWire};
use crate::bootstrap::AgentFactory;
use crate::interface::session::SessionManager;
use crate::project::{ProjectEntry, ProjectRegistry};
use crate::session::{
    SessionCatalog, SessionEntry, first_user_message, sanitize_title, title_prompt,
};
use crate::subagent::{SubagentInfo, SubagentManager};
use crate::todo::TaskView;

/// Pseudo project id for Chat mode: a project not in the registry, assembled from a
/// fixed chat root shared by desktop and server transports.
pub const CHAT_PROJECT_ID: &str = "__chat__";

/// Runtime for a single project: one assembled factory + its multi-session orchestrator.
pub struct ProjectRuntime {
    pub factory: Arc<AgentFactory>,
    pub sessions: SessionManager,
}

/// Event sink the transport supplies to receive wire events from the turn driver.
pub type EventSink = Arc<dyn Fn(AgentEventWire) + Send + Sync>;

/// Status sink for the **content-free** runtime-status stream (`status://event` on desktop,
/// `/api/status` SSE on the cloud). Deliberately separate from [`EventSink`]: the render channel
/// carries content and only fires on `AgentEvent`; the status channel ticks even when nothing else
/// happens (that is the whole point of stall detection). See `docs/agent-status.md` §6.1.
pub type StatusSink = Arc<dyn Fn(StateSnapshotWire) + Send + Sync>;

/// A shared, interface-agnostic multi-project runtime manager.
pub struct ProjectRuntimeManager {
    registry: Arc<Mutex<ProjectRegistry>>,
    runtimes: Arc<Mutex<HashMap<String, Arc<Mutex<ProjectRuntime>>>>>,
    cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    sink: EventSink,
    /// Content-free runtime-status fan-out (separate channel; no-op by default).
    status_sink: StatusSink,
    /// If `Some`, the `__chat__` pseudo project resolves to this root.
    /// If `None`, `__chat__` is rejected.
    chat_root: Option<PathBuf>,
}

/// Event pump / cancel key: unique across projects.
pub fn route_key(project: &str, session: &str) -> String {
    format!("{project}::{session}")
}

impl ProjectRuntimeManager {
    pub fn new(
        registry: Arc<Mutex<ProjectRegistry>>,
        chat_root: Option<PathBuf>,
        sink: EventSink,
    ) -> Self {
        Self {
            registry,
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            sink,
            status_sink: Arc::new(|_| {}),
            chat_root,
        }
    }

    /// Install the content-free status sink (desktop emits `status://event`; the server fans out
    /// `/api/status`). Kept as a builder so existing `new()` call sites are untouched.
    pub fn with_status_sink(mut self, sink: StatusSink) -> Self {
        self.status_sink = sink;
        self
    }

    /// Register (or refresh) a project at `working_dir` so the server always has at
    /// least one project to show. Best-effort: returns the entry, or an error string.
    pub async fn register_default(&self, working_dir: PathBuf) -> Result<ProjectEntry, String> {
        self.registry
            .lock()
            .await
            .add(&working_dir, None)
            .map_err(|e| e.to_string())
    }

    /// Get (lazily loading if needed) the given project's runtime and refresh its
    /// `last_opened_ms`.
    pub async fn ensure_runtime(
        &self,
        project: &str,
    ) -> Result<Arc<Mutex<ProjectRuntime>>, String> {
        if let Some(rt) = self.runtimes.lock().await.get(project).cloned() {
            if project != CHAT_PROJECT_ID {
                self.registry.lock().await.touch(project);
            }
            return Ok(rt);
        }
        // Chat pseudo project: bypass the registry, assemble from the chat root.
        if project == CHAT_PROJECT_ID {
            let root = self
                .chat_root
                .clone()
                .ok_or_else(|| "chat project is not available in this mode".to_owned())?;
            let runtime = tokio::task::spawn_blocking(move || {
                let _ = std::fs::create_dir_all(&root);
                build_runtime(root)
            })
            .await
            .map_err(|e| e.to_string())??;
            let rt = {
                let mut map = self.runtimes.lock().await;
                let entry = map
                    .entry(project.to_owned())
                    .or_insert_with(|| Arc::new(Mutex::new(runtime)));
                Arc::clone(entry)
            };
            return Ok(rt);
        }
        let entry = self
            .registry
            .lock()
            .await
            .get(project)
            .cloned()
            .ok_or_else(|| format!("project not found: {project}"))?;
        let path = entry.path.clone();
        let runtime = tokio::task::spawn_blocking(move || build_runtime(path))
            .await
            .map_err(|e| e.to_string())??;
        let rt = {
            let mut map = self.runtimes.lock().await;
            let entry = map
                .entry(project.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(runtime)));
            Arc::clone(entry)
        };
        self.registry.lock().await.touch(project);
        Ok(rt)
    }

    // -----------------------------------------------------------------------
    // Project commands
    // -----------------------------------------------------------------------

    pub async fn list_projects(&self, include_archived: bool) -> Vec<ProjectEntryWire> {
        let guard = self.registry.lock().await;
        guard
            .list(include_archived)
            .into_iter()
            .map(ProjectEntryWire::from)
            .collect()
    }

    /// Register a project from a server-side path (cloud `agent_add_project { path, name? }`).
    /// Canonicalises the path, dedupes via the registry, assembles its runtime eagerly.
    pub async fn add_project(
        &self,
        path: PathBuf,
        name: Option<String>,
    ) -> Result<ProjectEntryWire, String> {
        let entry = self
            .registry
            .lock()
            .await
            .add(&path, name)
            .map_err(|e| e.to_string())?;
        let root = entry.path.clone();
        let runtime = tokio::task::spawn_blocking(move || build_runtime(root))
            .await
            .map_err(|e| e.to_string())??;
        self.runtimes
            .lock()
            .await
            .insert(entry.id.clone(), Arc::new(Mutex::new(runtime)));
        Ok(ProjectEntryWire::from(&entry))
    }

    pub async fn rename_project(&self, project: &str, name: &str) -> Result<(), String> {
        self.registry
            .lock()
            .await
            .rename(project, name)
            .map_err(|e| e.to_string())
    }

    pub async fn archive_project(&self, project: &str) -> Result<(), String> {
        self.registry
            .lock()
            .await
            .archive(project)
            .map_err(|e| e.to_string())?;
        self.runtimes.lock().await.remove(project);
        Ok(())
    }

    pub async fn restore_project(&self, project: &str) -> Result<(), String> {
        self.registry
            .lock()
            .await
            .restore(project)
            .map_err(|e| e.to_string())
    }

    // -----------------------------------------------------------------------
    // Session commands
    // -----------------------------------------------------------------------

    pub async fn list_sessions(&self, project: &str) -> Result<Vec<SessionEntryWire>, String> {
        let runtime = self.ensure_runtime(project).await?;
        let catalog = Arc::clone(&runtime.lock().await.factory.session_catalog);
        let entries = tokio::task::spawn_blocking(move || catalog.list())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let guard = runtime.lock().await;
        Ok(entries
            .into_iter()
            .map(|entry| {
                let running = guard.sessions.is_running(&entry.name);
                SessionEntryWire::from_entry(entry, running)
            })
            .collect())
    }

    pub async fn current_session(&self, project: &str) -> Result<String, String> {
        let runtime = self.ensure_runtime(project).await?;
        let guard = runtime.lock().await;
        guard
            .sessions
            .active_name()
            .map(str::to_owned)
            .ok_or_else(|| "no active session".to_owned())
    }

    pub async fn new_session(
        &self,
        project: &str,
        title: Option<String>,
    ) -> Result<String, String> {
        let runtime = self.ensure_runtime(project).await?;
        let mut guard = runtime.lock().await;
        guard.sessions.create_new(title.as_deref())
    }

    pub async fn switch_session(&self, project: &str, name: &str) -> Result<String, String> {
        let runtime = self.ensure_runtime(project).await?;
        let mut guard = runtime.lock().await;
        guard.sessions.switch_to(name)
    }

    pub async fn rename_session(
        &self,
        project: &str,
        name: &str,
        title: &str,
    ) -> Result<(), String> {
        let runtime = self.ensure_runtime(project).await?;
        let catalog = Arc::clone(&runtime.lock().await.factory.session_catalog);
        let name = name.to_owned();
        let title = title.to_owned();
        tokio::task::spawn_blocking(move || catalog.rename(&name, &title))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }

    pub async fn delete_session(&self, project: &str, name: &str) -> Result<(), String> {
        let runtime = self.ensure_runtime(project).await?;
        runtime.lock().await.sessions.delete_session(name)?;
        let catalog = Arc::clone(&runtime.lock().await.factory.session_catalog);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || catalog.delete(&name))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }

    pub async fn move_session(
        &self,
        project: &str,
        name: &str,
        target_project: &str,
    ) -> Result<(), String> {
        if project == target_project {
            return Err("source and target projects are the same".to_owned());
        }
        let source = self.ensure_runtime(project).await?;
        let target = self.ensure_runtime(target_project).await?;
        {
            let mut guard = source.lock().await;
            guard.sessions.forget_session(name)?;
        }
        let dest_dir = target
            .lock()
            .await
            .factory
            .session_catalog
            .storage_dir()
            .ok_or_else(|| "the target project does not support a session directory".to_owned())?;
        let catalog = Arc::clone(&source.lock().await.factory.session_catalog);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || catalog.move_to(&name, &dest_dir))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }

    // -----------------------------------------------------------------------
    // Turn driver
    // -----------------------------------------------------------------------

    /// Compose prompt + references, then drive one turn.
    pub async fn send(
        &self,
        project: &str,
        session: Option<String>,
        text: String,
        references: Vec<String>,
        images: Vec<String>,
    ) -> Result<(), String> {
        let prompt = compose_prompt(&text, &references);
        let user_content = user_content(&prompt, &images);
        self.spawn_turn(project, session, text, user_content).await
    }

    /// Rewind the last turn, then send the edited text as a new turn.
    pub async fn resend(
        &self,
        project: &str,
        session: Option<String>,
        text: String,
        references: Vec<String>,
        images: Vec<String>,
    ) -> Result<(), String> {
        let runtime = self.ensure_runtime(project).await?;
        {
            let mut guard = runtime.lock().await;
            let session_name = match session.clone() {
                Some(name) => name,
                None => guard
                    .sessions
                    .active_name()
                    .map(str::to_owned)
                    .ok_or_else(|| "current session is not yet persisted".to_owned())?,
            };
            if guard.sessions.rewind_last_turn(&session_name).is_none() {
                return Err("no message to edit".to_owned());
            }
        }
        let prompt = compose_prompt(&text, &references);
        let user_content = user_content(&prompt, &images);
        self.spawn_turn(project, session, text, user_content).await
    }

    /// Take the session's `Agent`, register the cancel handle, and spawn the run task.
    pub async fn spawn_turn(
        &self,
        project: &str,
        session: Option<String>,
        text: String,
        user_content: Content,
    ) -> Result<(), String> {
        let runtime = self.ensure_runtime(project).await?;
        let (mut agent, memory, session_name, status) = {
            let mut guard = runtime.lock().await;
            let session_name = match session {
                Some(name) => name,
                None => guard
                    .sessions
                    .active_name()
                    .map(str::to_owned)
                    .ok_or_else(|| "current session is not yet persisted".to_owned())?,
            };
            guard
                .sessions
                .session(&session_name)
                .ok_or("session not found")?
                .model_availability()?;
            let agent = guard
                .sessions
                .begin_turn(&session_name, text.clone())
                .ok_or_else(|| "agent is currently running".to_owned())?;
            let memory = guard
                .sessions
                .session(&session_name)
                .and_then(|s| s.memory.clone());
            if let Some(memory) = memory.as_ref() {
                memory.set_query(text.as_str());
            }
            let status = guard
                .sessions
                .session(&session_name)
                .map(|s| Arc::clone(&s.status))
                .ok_or_else(|| "session not found".to_owned())?;
            (agent, memory, session_name, status)
        };

        let catalog = Arc::clone(&runtime.lock().await.factory.session_catalog);
        let runtime_for_task = Arc::clone(&runtime);
        let sink = Arc::clone(&self.sink);
        let status_sink = Arc::clone(&self.status_sink);
        let cancels = Arc::clone(&self.cancels);
        let key = route_key(project, &session_name);
        let project_owned = project.to_owned();
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        self.cancels.lock().await.insert(key.clone(), cancel_tx);

        tokio::spawn(async move {
            // Safety net (`docs/agent-status.md` §5.6): if this task is aborted/dropped mid-turn the
            // guard flips the keeper to `Unreachable` + `stale`; disarmed on the normal path below.
            let mut turn_guard = crate::status::TurnGuard::new(Arc::clone(&status));
            emit_status(&status_sink, &project_owned, &session_name, &status);
            let terminal = guard_turn(async {
                let mut cancel_rx = cancel_rx;
                let mut stream = agent.run_stream_content(user_content);
                let mut hb = tokio::time::interval(std::time::Duration::from_secs(1));
                if let Some(memory) = memory.as_ref() {
                    tokio::select! {
                        biased;
                        _ = &mut cancel_rx => return Ok(cancelled_event()),
                        _ = memory.prefetch_query_vector() => {}
                    }
                }
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut cancel_rx => return Ok(cancelled_event()),
                        // Heartbeat: the only thing that ticks when no AgentEvent arrives, so a silent
                        // turn still flips Running -> Stalled (docs §5.2).
                        _ = hb.tick() => {
                            status.tick();
                            emit_status(&status_sink, &project_owned, &session_name, &status);
                        }
                        event = stream.next() => {
                            let event = event
                                .ok_or_else(|| "Agent stream ended unexpectedly; conversation preserved.".to_owned())?
                                .map_err(|error| error.to_string())?;
                            if matches!(&event, AgentEvent::Finished(_)) {
                                return Ok(event);
                            }
                            let is_todo = matches!(&event, AgentEvent::ToolFinished { name, .. } if name == "todo");
                            {
                                let mut rt = runtime_for_task.lock().await;
                                rt.sessions.apply_event(&session_name, Ok(event.clone()));
                                // The cloud cursor and view must advance under the same lock.
                                sink_event(&sink, &project_owned, &session_name, event);
                                if is_todo {
                                    let todo = rt.sessions.session(&session_name).and_then(super::session::Session::todo_view);
                                    sink_todo(&sink, &project_owned, &session_name, todo);
                                }
                            }
                            emit_status(&status_sink, &project_owned, &session_name, &status);
                        }
                    }
                }
            })
            .await;

            let title_input = matches!(&terminal, Ok(AgentEvent::Finished(result))
                if matches!(result.stop_reason, StopReason::Completed))
            .then(|| first_user_message(agent.messages()))
            .flatten()
            .map(|first| (agent.model_config().clone(), first));
            let messages = agent.finish_interrupted_turn();
            cancels.lock().await.remove(&key);

            {
                let mut rt = runtime_for_task.lock().await;
                for message in messages {
                    rt.sessions
                        .apply_event(&session_name, Ok(AgentEvent::MessageAdded(message)));
                }
                rt.sessions.restore_agent(&session_name, agent);
                rt.sessions.apply_event(&session_name, terminal.clone());
                match &terminal {
                    Ok(event) => sink_event(&sink, &project_owned, &session_name, event.clone()),
                    Err(message) => sink_error(&sink, &project_owned, &session_name, message),
                }
            }
            emit_status(&status_sink, &project_owned, &session_name, &status);

            turn_guard.disarm();
            if let Some((config, first)) = title_input {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    auto_title_session(config, first, &catalog, &session_name),
                )
                .await;
            }
        });

        Ok(())
    }

    pub async fn cancel(&self, project: &str, session: &str) {
        let key = route_key(project, session);
        if let Some(tx) = self.cancels.lock().await.remove(&key) {
            let _ = tx.send(());
        }
    }

    // -----------------------------------------------------------------------
    // Subscription
    // -----------------------------------------------------------------------

    /// Return a snapshot of the session view + a live event receiver.
    ///
    /// Subscribe and snapshot happen **under the same lock** so nothing is lost or
    /// duplicated: the snapshot covers before-subscribe, the receiver covers after.
    pub async fn subscribe(
        &self,
        project: &str,
        session: &str,
    ) -> Result<
        (
            broadcast::Receiver<Result<AgentEvent, String>>,
            SessionSnapshotWire,
        ),
        String,
    > {
        self.subscribe_with_cursor(project, session, || 0)
            .await
            .map(|(rx, snapshot, _)| (rx, snapshot))
    }

    pub async fn subscribe_with_cursor(
        &self,
        project: &str,
        session: &str,
        cursor: impl FnOnce() -> u64,
    ) -> Result<
        (
            broadcast::Receiver<Result<AgentEvent, String>>,
            SessionSnapshotWire,
            u64,
        ),
        String,
    > {
        let runtime = self.ensure_runtime(project).await?;
        let guard = runtime.lock().await;
        let rx = guard
            .sessions
            .subscribe(session)
            .ok_or_else(|| format!("session not found: {session}"))?;
        let items = guard
            .sessions
            .snapshot(session)
            .ok_or_else(|| format!("session not found: {session}"))?;
        let running = guard.sessions.is_running(session);
        let todo = guard
            .sessions
            .session(session)
            .and_then(super::session::Session::todo_view);
        let snapshot = SessionSnapshotWire::new(session.to_owned(), items, running, todo);
        Ok((rx, snapshot, cursor()))
    }

    // -----------------------------------------------------------------------
    // Model commands
    // -----------------------------------------------------------------------

    pub async fn model_name(&self, project: &str) -> Result<String, String> {
        let runtime = self.ensure_runtime(project).await?;
        let guard = runtime.lock().await;
        if let Some(key) = &guard.sessions.active().model_selection {
            return Ok(key.clone());
        }
        Ok(guard
            .sessions
            .active_model_name()
            .map(str::to_owned)
            .unwrap_or_default())
    }

    pub async fn list_models(&self, project: &str) -> Result<Vec<ModelEntryWire>, String> {
        let runtime = self.ensure_runtime(project).await?;
        let catalog = Arc::clone(&runtime.lock().await.factory.model_catalog);
        let entries = catalog.list().await;
        Ok(entries
            .into_iter()
            .map(|entry| ModelEntryWire {
                label: entry.label,
                value: entry.value,
                provider: entry.provider,
            })
            .collect())
    }

    pub async fn set_model(
        &self,
        project: &str,
        model: &str,
        target_session: Option<&str>,
    ) -> Result<(), String> {
        let runtime = self.ensure_runtime(project).await?;
        let (factory, session_name) = {
            let guard = runtime.lock().await;
            (
                Arc::clone(&guard.factory),
                target_session
                    .map(str::to_string)
                    .or_else(|| guard.sessions.active_name().map(str::to_string))
                    .ok_or("no active session")?,
            )
        };
        let entry = factory
            .model_catalog
            .list()
            .await
            .into_iter()
            .find(|entry| entry.value == model);
        if model.starts_with("provider:") && entry.is_none() {
            return Err("服务商或模型已被移除，请刷新列表".into());
        }
        // Resolve the full model config through the project factory (tool protocol etc.).
        let config = entry.as_ref().map(|entry| factory.model_config_for(entry));
        let mut guard = runtime.lock().await;
        let session = guard
            .sessions
            .session_mut(&session_name)
            .ok_or("session no longer exists")?;
        let agent = session
            .agent
            .as_ref()
            .ok_or_else(|| "agent is currently running".to_owned())?;
        let config = if let Some(config) = config {
            config
        } else {
            let mut config = agent.model_config().clone();
            config.model = model.into();
            config
        };
        let key = entry
            .as_ref()
            .filter(|e| e.configured.is_some())
            .map(|e| e.value.clone());
        session.select_model(config, key)
    }

    // -----------------------------------------------------------------------
    // Workspace files / skills
    // -----------------------------------------------------------------------

    pub async fn search_files(
        &self,
        project: &str,
        query: String,
    ) -> Result<Vec<FileEntryWire>, String> {
        let runtime = self.ensure_runtime(project).await?;
        let root = runtime.lock().await.factory.working_dir.clone();
        let entries = tokio::task::spawn_blocking(move || {
            crate::workspace_search::search_files(&root, &query, None)
        })
        .await
        .map_err(|e| e.to_string())?;
        Ok(entries
            .into_iter()
            .map(|entry| FileEntryWire {
                path: entry.path,
                name: entry.name,
                kind: match entry.kind {
                    crate::workspace_search::FileKind::Dir => "dir",
                    crate::workspace_search::FileKind::File => "file",
                },
            })
            .collect())
    }

    pub async fn list_skills(&self, project: &str) -> Result<Vec<SkillEntryWire>, String> {
        let runtime = self.ensure_runtime(project).await?;
        let guard = runtime.lock().await;
        let skills = guard
            .sessions
            .active()
            .skills
            .as_ref()
            .map(|skills| skills.entries())
            .unwrap_or_default();
        Ok(skills
            .into_iter()
            .map(|(name, description)| SkillEntryWire { name, description })
            .collect())
    }

    pub async fn activate_skill(
        &self,
        project: &str,
        session: &str,
        name: &str,
    ) -> Result<(), String> {
        let runtime = self.ensure_runtime(project).await?;
        let guard = runtime.lock().await;
        let session_ref = guard
            .sessions
            .session(session)
            .ok_or_else(|| format!("session not found: {session}"))?;
        let skills = session_ref
            .skills
            .as_ref()
            .ok_or_else(|| "the current session does not support skills".to_owned())?;
        skills
            .activate(name)
            .map(|_| ())
            .map_err(|error| match error {
                crate::skills::runtime::ActivateError::UnknownSkill(name) => {
                    format!("unknown skill: {name}")
                }
                crate::skills::runtime::ActivateError::ReadFailed(reason) => reason,
            })
    }

    // -----------------------------------------------------------------------
    // History / subagents
    // -----------------------------------------------------------------------

    pub async fn load_history(&self, project: &str) -> Result<Vec<HistoryMessageWire>, String> {
        let runtime = self.ensure_runtime(project).await?;
        let guard = runtime.lock().await;
        let agent = guard
            .sessions
            .active_agent()
            .ok_or_else(|| "agent is currently running".to_owned())?;
        Ok(history_from_messages(agent.messages()))
    }

    async fn subagent_manager(
        &self,
        project: &str,
        session: &str,
    ) -> Result<Arc<SubagentManager>, String> {
        let runtime = self.ensure_runtime(project).await?;
        let guard = runtime.lock().await;
        guard
            .sessions
            .session(session)
            .and_then(|s| s.subagents.clone())
            .ok_or_else(|| format!("session has no subagents: {session}"))
    }

    pub async fn list_subagents(
        &self,
        project: &str,
        session: &str,
    ) -> Result<Vec<SubagentSummaryWire>, String> {
        let manager = self.subagent_manager(project, session).await?;
        Ok(manager
            .list()
            .iter()
            .map(SubagentSummaryWire::from_info)
            .collect())
    }

    pub async fn load_subagent(
        &self,
        project: &str,
        session: &str,
        id: &str,
    ) -> Result<Vec<HistoryMessageWire>, String> {
        let manager = self.subagent_manager(project, session).await?;
        Ok(history_from_messages(&manager.load_transcript(id)))
    }

    pub async fn cancel_subagent(
        &self,
        project: &str,
        session: &str,
        id: &str,
    ) -> Result<(), String> {
        let manager = self.subagent_manager(project, session).await?;
        let _ = manager.cancel(id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Runtime assembly
// ---------------------------------------------------------------------------

/// Build a project runtime from an existing factory (resume the most recent session,
/// else lazily open an empty one).
pub fn runtime_from_factory(factory: Arc<AgentFactory>) -> Result<ProjectRuntime, String> {
    let catalog = Arc::clone(&factory.session_catalog);
    let latest = catalog
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .next();
    let (name, store, from_disk) = if let Some(entry) = latest {
        let store = catalog.open(&entry.name).map_err(|e| e.to_string())?;
        (entry.name, store, true)
    } else {
        let (entry, store) = catalog.create_lazy(None).map_err(|e| e.to_string())?;
        (entry.name, store, false)
    };
    let log = store.load().map_err(|e| e.to_string())?;
    let built = factory.build_agent(log).map_err(|e| e.to_string())?;
    let mut sessions = SessionManager::with_factory(
        built.agent,
        Some(name),
        catalog,
        Arc::clone(&factory),
        Some(store),
        Some(built.memory),
        Some(built.skills),
        Some(built.todo),
        built.subagents,
    );
    if from_disk {
        sessions.active_mut().rebuild_items_from_agent();
    }
    Ok(ProjectRuntime { factory, sessions })
}

/// Assemble a new factory for an arbitrary directory and build its runtime.
pub fn build_runtime(root: PathBuf) -> Result<ProjectRuntime, String> {
    let factory = Arc::new(AgentFactory::assemble(root).map_err(|e| e.to_string())?);
    runtime_from_factory(factory)
}

// ---------------------------------------------------------------------------
// Sink helpers
// ---------------------------------------------------------------------------

fn sink_event(sink: &EventSink, project: &str, session: &str, event: AgentEvent) {
    if let Some(wire) = AgentEventWire::from_event(event).map(|w| w.with_route(project, session)) {
        sink(wire);
    }
}

fn emit_status(
    sink: &StatusSink,
    project: &str,
    session: &str,
    keeper: &crate::status::StateKeeper,
) {
    sink(StateSnapshotWire::from_snapshot(&keeper.snapshot()).with_route(project, session));
}

fn sink_error(sink: &EventSink, project: &str, session: &str, message: &str) {
    sink(AgentEventWire::Error {
        project: Some(project.to_owned()),
        session: Some(session.to_owned()),
        message: message.to_owned(),
    });
}

fn sink_todo(sink: &EventSink, project: &str, session: &str, todo: Option<TaskView>) {
    sink(AgentEventWire::TodoUpdated {
        project: Some(project.to_owned()),
        session: Some(session.to_owned()),
        todo: todo.map(TaskViewWire::from),
    });
}

fn cancelled_event() -> AgentEvent {
    AgentEvent::Finished(RunResult {
        messages: Vec::new(),
        stop_reason: StopReason::Cancelled,
        usage: Usage::default(),
    })
}

async fn guard_turn(
    future: impl std::future::Future<Output = Result<AgentEvent, String>>,
) -> Result<AgentEvent, String> {
    AssertUnwindSafe(future)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            Err("Task crashed; conversation preserved. Check any interrupted tool's effects before continuing.".to_owned())
        })
}

async fn auto_title_session(
    config: shirley_agent_sdk::ModelConfig,
    first: String,
    catalog: &Arc<dyn SessionCatalog>,
    name: &str,
) {
    let probe_catalog = Arc::clone(catalog);
    let probe_name = name.to_owned();
    let already_named = tokio::task::spawn_blocking(move || {
        probe_catalog
            .list()
            .ok()
            .and_then(|entries| entries.into_iter().find(|entry| entry.name == probe_name))
            .is_some_and(|entry| entry.label != entry.name)
    })
    .await
    .unwrap_or(false);
    if already_named {
        return;
    }
    let raw = if config.model == "deepseek-web" {
        first.chars().take(20).collect::<String>()
    } else {
        let Ok(agent) = Agent::builder().model_config(config).build() else {
            return;
        };
        let Ok(raw) = agent.complete(&title_prompt(&first)).await else {
            return;
        };
        raw
    };
    let Some(title) = sanitize_title(&raw) else {
        return;
    };
    let write_catalog = Arc::clone(catalog);
    let write_name = name.to_owned();
    let _ = tokio::task::spawn_blocking(move || write_catalog.rename(&write_name, &title)).await;
}

// ---------------------------------------------------------------------------
// Wire DTOs (aligned with `web/src/lib/bridge.ts`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ProjectEntryWire {
    pub id: String,
    pub name: String,
    pub path: String,
    pub archived: bool,
    pub last_opened_ms: u64,
}

impl From<&ProjectEntry> for ProjectEntryWire {
    fn from(entry: &ProjectEntry) -> Self {
        Self {
            id: entry.id.clone(),
            name: entry.name.clone(),
            path: entry.path.to_string_lossy().into_owned(),
            archived: entry.archived,
            last_opened_ms: entry.last_opened_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionEntryWire {
    pub name: String,
    pub label: String,
    pub preview: String,
    pub modified_ms: u64,
    pub turns: usize,
    pub running: bool,
}

impl SessionEntryWire {
    pub fn from_entry(entry: SessionEntry, running: bool) -> Self {
        Self {
            name: entry.name,
            label: entry.label,
            preview: entry.preview,
            modified_ms: entry.modified_ms,
            turns: entry.turns,
            running,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelEntryWire {
    pub label: String,
    pub value: String,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEntryWire {
    pub path: String,
    pub name: String,
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillEntryWire {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryMessageWire {
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubagentSummaryWire {
    pub id: String,
    pub kind: &'static str,
    pub task: String,
    pub status: &'static str,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub elapsed_ms: u64,
    pub summary: String,
    pub stop_reason: String,
}

impl SubagentSummaryWire {
    pub fn from_info(info: &SubagentInfo) -> Self {
        let (summary, stop_reason) = match &info.result {
            Some(result) => (result.summary.clone(), result.stop_reason.clone()),
            None => (String::new(), String::new()),
        };
        Self {
            id: info.id.clone(),
            kind: info.kind.as_str(),
            task: info.task.clone(),
            status: info.status.as_str(),
            started_ms: info.started_ms,
            finished_ms: info.finished_ms,
            elapsed_ms: info.elapsed_ms(),
            summary,
            stop_reason,
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt / history helpers
// ---------------------------------------------------------------------------

pub fn history_from_messages(messages: &[Message]) -> Vec<HistoryMessageWire> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(HistoryMessageWire {
                role: "user",
                content: content.as_text(),
            }),
            Message::Assistant { content, .. } => content
                .as_ref()
                .filter(|text| !text.trim().is_empty())
                .map(|text| HistoryMessageWire {
                    role: "assistant",
                    content: text.clone(),
                }),
            _ => None,
        })
        .collect()
}

pub fn compose_prompt(text: &str, references: &[String]) -> String {
    if references.is_empty() {
        return text.to_owned();
    }
    let mut prompt = String::from("Referenced workspace files/directories:\n");
    for reference in references {
        prompt.push_str("- ");
        prompt.push_str(reference);
        prompt.push('\n');
    }
    prompt.push('\n');
    prompt.push_str(text);
    prompt
}

pub fn user_content(prompt: &str, images: &[String]) -> Content {
    let mut parts: Vec<ContentPart> = Vec::new();
    if !prompt.is_empty() {
        parts.push(ContentPart::Text {
            text: prompt.to_owned(),
        });
    }
    for url in images {
        parts.push(ContentPart::Image(ImagePart::url(url.clone())));
    }
    Content::from(parts)
}
