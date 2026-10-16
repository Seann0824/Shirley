//! Tauri 2 shell: wires the multi-session `Agent`s built by `AgentFactory` into the webview.
//!
//! # Multiple projects (multiple workspaces)
//!
//! Shirley as a standalone app is not bound to a single directory: the user can
//! register multiple workspaces as "projects", each with its own sessions. The
//! registry ([`ProjectRegistry`]) only records "where the project is", persisted to
//! `<config_dir>/shirley/projects.json`; **one [`AgentFactory`] per project** (each
//! reading that project's `<root>/.shirley/config.toml`), and session logs still go
//! to each project's own `<root>/.shirley/sessions/` (traveling with the repo).
//!
//! "Remove project" = **archive**: only sets the `archived` flag, never deletes disk
//! data, and can be restored from the archive list.
//!
//! # Event contract (aligned with `web/src/lib/bridge.ts`)
//!
//! - command `agent_send { project, session?, text, references? }`: starts one
//!   `run_stream` turn for the given session; events are pushed via `agent://event`
//!   tagged with source `project` + `session`.
//! - command `agent_subscribe { project, session }`: subscribe to a session (returns a
//!   snapshot + starts a pump).
//! - command `agent_list_projects { archived? }` / `agent_add_project` (native folder picker)
//!   / `agent_rename_project` / `agent_archive_project` / `agent_restore_project`.
//! - command `agent_cancel { project, session }`: cancel the running turn of the given
//!   session — sends a cancel signal to the task driving that session's `run_stream`;
//!   the task drops the stream, rolls back Agent state, and finishes with
//!   `Finished(Cancelled)`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use std::panic::AssertUnwindSafe;
use serde::Serialize;
use shirley_agent_sdk::{Agent, AgentEvent, Content, ContentPart, ImagePart, Message, RunResult, StopReason};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::{Mutex, oneshot};

use super::wire::{AgentEventWire, SessionSnapshotWire, TaskViewWire};
use crate::bootstrap::AgentFactory;
use crate::interface::session::SessionManager;
use crate::project::{ProjectEntry, ProjectRegistry};
use crate::subagent::{SubagentInfo, SubagentManager};
use crate::session::{
    SessionCatalog, SessionEntry, first_user_message, sanitize_title, title_prompt,
};

/// The single event name the webview listens on.
const EVENT_NAME: &str = "agent://event";

/// Pseudo project id for Chat mode: **not associated with any user project**, but
/// reuses the same project-scoped commands (`agent_send` / `agent_subscribe` /
/// `agent_list_sessions` ...). It never enters [`ProjectRegistry`], so it never shows
/// up in the project list; sessions land in
/// `<config_dir>/shirley/chat/.shirley/sessions/` (same root as global memory, not
/// bound to a repo).
const CHAT_PROJECT_ID: &str = "__chat__";

/// Chat mode workspace root: `<config_dir>/shirley/chat`. Falls back to a temp dir when `config_dir` is unavailable.
fn chat_root() -> PathBuf {
    dirs::config_dir()
        .map(|dir| dir.join("shirley").join("chat"))
        .unwrap_or_else(|| std::env::temp_dir().join("shirley-chat"))
}

/// Runtime for a single project: one assembled factory + its multi-session orchestrator.
///
/// The session log directory / system prompt / memory root all follow `factory` (i.e.
/// that project's `working_dir`); so different projects are naturally isolated and
/// share no mutable state.
struct ProjectRuntime {
    factory: Arc<AgentFactory>,
    sessions: SessionManager,
}

/// Tauri-managed runtime state (multiple projects).
///
/// - `registry`: the project registry (which directories are projects).
/// - `runtimes`: runtimes of **opened** projects, lazily loaded by project id
///   (`ensure_runtime`). Closing / archiving a project removes it from the map and
///   releases its in-memory sessions.
/// - `pumps`: the event pump handle for each "project::session".
struct DesktopState {
    registry: Arc<Mutex<ProjectRegistry>>,
    runtimes: Arc<Mutex<HashMap<String, Arc<Mutex<ProjectRuntime>>>>>,
    pumps: Arc<Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>>,
    /// Cancel handle for each "project::session" while a turn is running. Inserted
    /// when `agent_send` starts a turn, removed when it ends (normal / cancelled /
    /// error); `agent_cancel` takes it out and `send(())` to interrupt. The key
    /// matches `pumps` (`route_key`).
    cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}

/// Event pump key: unique across projects (session names are unique within a project, project ids globally).
fn route_key(project: &str, session: &str) -> String {
    format!("{project}::{session}")
}

/// Build a project runtime from an existing factory.
///
/// **Resume semantics**: prefer opening the project's **most recently modified**
/// session (`SessionCatalog::list` is sorted by `modified_ms` descending) and restore
/// it as the foreground session; only when the project has **no sessions at all** do
/// we lazily open an empty session (naming it only, without writing to disk).
///
/// We must "resume the latest session" rather than unconditionally create a new one:
/// `ensure_runtime` calls this on the project's **first lazy load** (triggered by the
/// frontend switching projects / the sidebar polling `agent_list_sessions`). If it
/// always did `create_lazy(None)`, `agent_current_session` would report a **freshly
/// conjured empty session** to the frontend, manifesting as "switching projects turns
/// the selected session into new session".
fn runtime_from_factory(factory: Arc<AgentFactory>) -> Result<ProjectRuntime, String> {
    let catalog = factory.session_catalog.clone();
    // Most recently modified session first; only create a lazy empty session when there are none.
    let latest = catalog
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .next();
    let (name, store, from_disk) = match latest {
        Some(entry) => {
            let store = catalog.open(&entry.name).map_err(|e| e.to_string())?;
            (entry.name, store, true)
        }
        None => {
            let (entry, store) = catalog.create_lazy(None).map_err(|e| e.to_string())?;
            (entry.name, store, false)
        }
    };
    let log = store.load().map_err(|e| e.to_string())?;
    let built = factory.build_agent(log).map_err(|e| e.to_string())?;
    let mut sessions = SessionManager::with_factory(
        built.agent,
        Some(name),
        catalog,
        factory.clone(),
        Some(store),
        Some(built.memory),
        Some(built.skills),
        Some(built.todo),
        built.subagents,
    );
    // A session restored from disk: its `Agent` already has the full history, but the
    // view items are still empty — rebuild once so snapshots / rendering have content
    // (same as `SessionManager::switch_to`'s resume path).
    if from_disk {
        sessions.active_mut().rebuild_items_from_agent();
    }
    Ok(ProjectRuntime { factory, sessions })
}

/// Assemble a new factory for an arbitrary directory and build its runtime ("new project").
fn build_runtime(root: PathBuf) -> Result<ProjectRuntime, String> {
    let factory = Arc::new(AgentFactory::assemble(root).map_err(|e| e.to_string())?);
    runtime_from_factory(factory)
}

/// Get (lazily loading if needed) the given project's runtime and refresh its `last_opened_ms`.
///
/// Not loaded → look up the path in the registry, `build_runtime`, then insert. Returns
/// an error if the project does not exist / the directory is invalid.
async fn ensure_runtime(
    state: &DesktopState,
    project: &str,
) -> Result<Arc<Mutex<ProjectRuntime>>, String> {
    if let Some(rt) = state.runtimes.lock().await.get(project).cloned() {
        // The Chat pseudo project is not in the registry, so there is no `last_opened_ms` to refresh.
        if project != CHAT_PROJECT_ID {
            state.registry.lock().await.touch(project);
        }
        return Ok(rt);
    }
    // Chat mode: the pseudo project bypasses the registry and assembles a runtime
    // directly from `<config_dir>/shirley/chat`. This lets all project-scoped commands
    // be reused unchanged (session logs also land in that directory's `.shirley`).
    if project == CHAT_PROJECT_ID {
        let root = chat_root();
        let runtime = tauri::async_runtime::spawn_blocking(move || {
            let _ = std::fs::create_dir_all(&root);
            build_runtime(root)
        })
        .await
        .map_err(|e| e.to_string())??;
        let rt = {
            let mut map = state.runtimes.lock().await;
            map.entry(project.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(runtime)))
                .clone()
        };
        return Ok(rt);
    }
    let entry = state
        .registry
        .lock()
        .await
        .get(project)
        .cloned()
        .ok_or_else(|| format!("project not found: {project}"))?;
    let path = entry.path.clone();
    let runtime = tauri::async_runtime::spawn_blocking(move || build_runtime(path))
        .await
        .map_err(|e| e.to_string())??;
    let rt = {
        let mut map = state.runtimes.lock().await;
        map.entry(project.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(runtime)))
            .clone()
    };
    // Release the runtimes lock before touch (avoid nested registry/runtimes locking).
    state.registry.lock().await.touch(project);
    Ok(rt)
}

/// Model entry consumed by the frontend (aligned with `ModelEntry` in `web/src/lib/bridge.ts`).
#[derive(Debug, Clone, Serialize)]
struct ModelEntryWire {
    label: String,
    value: String,
    provider: String,
}

/// Entry selectable for `@` references (aligned with `FileEntry` in `web/src/lib/bridge.ts`).
#[derive(Debug, Clone, Serialize)]
struct FileEntryWire {
    path: String,
    name: String,
    kind: &'static str,
}

/// `/` skill candidate entry (aligned with `SkillEntry` in `web/src/lib/bridge.ts`).
#[derive(Debug, Clone, Serialize)]
struct SkillEntryWire {
    name: String,
    description: String,
}

/// Project entry consumed by the frontend (aligned with `ProjectEntry` in `web/src/lib/bridge.ts`).
#[derive(Debug, Clone, Serialize)]
struct ProjectEntryWire {
    id: String,
    name: String,
    path: String,
    archived: bool,
    last_opened_ms: u64,
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

/// Session entry consumed by the frontend (aligned with `SessionEntry` in `web/src/lib/bridge.ts`).
///
/// `label` is the display title (custom title first, otherwise falls back to `name`);
/// `preview` is the first user message; `modified_ms` / `turns` match Codex's resume
/// picker time and turn count.
#[derive(Debug, Clone, Serialize)]
struct SessionEntryWire {
    name: String,
    label: String,
    preview: String,
    modified_ms: u64,
    turns: usize,
    /// Whether this session is currently running a turn (active in the background) — the list shows a loading spinner based on this.
    running: bool,
}

impl From<SessionEntry> for SessionEntryWire {
    fn from(entry: SessionEntry) -> Self {
        Self {
            name: entry.name,
            label: entry.label,
            preview: entry.preview,
            modified_ms: entry.modified_ms,
            turns: entry.turns,
            running: false,
        }
    }
}

/// A history message replayed to the frontend when restoring a session (aligned with
/// `HistoryMessage` in `web/src/lib/bridge.ts`). Only exposes displayable User / Assistant bodies.
#[derive(Debug, Clone, Serialize)]
struct HistoryMessageWire {
    role: &'static str,
    content: String,
}

/// Extract displayable history from the `Agent`'s current message list (skips system / tool / context_summary).
fn history_from_messages(messages: &[Message]) -> Vec<HistoryMessageWire> {
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

/// Compose the message body and `@` references into the prompt sent to the Agent this turn.
fn compose_prompt(text: &str, references: &[String]) -> String {
    if references.is_empty() {
        return text.to_owned();
    }
    let mut prompt = String::new();
    prompt.push_str("Referenced workspace files/directories:\n");
    for reference in references {
        prompt.push_str("- ");
        prompt.push_str(reference);
        prompt.push('\n');
    }
    prompt.push('\n');
    prompt.push_str(text);
    prompt
}

/// Build the structured user turn: prompt text followed by pasted image parts.
///
/// `images` are `data:` URLs; each becomes an [`ImagePart`] with an inline (base64) source.
/// With no images the result is a single text part, so text-only turns keep the plain-string
/// fast path (and the KV prefix cache).
fn user_content(prompt: &str, images: &[String]) -> Content {
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

// ---------------------------------------------------------------------------
// Project commands
// ---------------------------------------------------------------------------

/// List projects (most recently opened first). `archived = true` includes archived projects (for the archive list).
#[tauri::command]
async fn agent_list_projects(
    state: State<'_, DesktopState>,
    archived: Option<bool>,
) -> Result<Vec<ProjectEntryWire>, String> {
    let include_archived = archived.unwrap_or(false);
    let guard = state.registry.lock().await;
    Ok(guard
        .list(include_archived)
        .into_iter()
        .map(ProjectEntryWire::from)
        .collect())
}

/// Open the native folder picker to create a new project; returns `None` if cancelled.
///
/// Already-registered directories are deduplicated by id: if archived, "restore and
/// rename"; otherwise only refresh the open time — never double-register.
#[tauri::command]
async fn agent_add_project(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Option<ProjectEntryWire>, String> {
    // The native dialog is a blocking call; move it to the blocking thread pool (it internally hops back to the main thread to display).
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|file| file.into_path().ok()) else {
        return Ok(None);
    };

    let entry = state
        .registry
        .lock()
        .await
        .add(path, None)
        .map_err(|e| e.to_string())?;
    // Assemble the runtime immediately (with a lazy empty session) so the user can start chatting in this project right away.
    let root = entry.path.clone();
    let runtime = tauri::async_runtime::spawn_blocking(move || build_runtime(root))
        .await
        .map_err(|e| e.to_string())??;
    state
        .runtimes
        .lock()
        .await
        .insert(entry.id.clone(), Arc::new(Mutex::new(runtime)));
    Ok(Some(ProjectEntryWire::from(&entry)))
}

/// Rename a project (display name only; path / session data untouched).
#[tauri::command]
async fn agent_rename_project(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
) -> Result<(), String> {
    state
        .registry
        .lock()
        .await
        .rename(&project, &name)
        .map_err(|e| e.to_string())
}

/// Archive a project: only sets the flag, **does not delete disk data**; also releases its in-memory runtime.
#[tauri::command]
async fn agent_archive_project(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<(), String> {
    state
        .registry
        .lock()
        .await
        .archive(&project)
        .map_err(|e| e.to_string())?;
    // Archive = close: drop the runtime (session Agents / in-memory state released, disk data untouched).
    state.runtimes.lock().await.remove(&project);
    Ok(())
}

/// Restore a project from the archive (refreshes the open time; the runtime lazily loads on next open).
#[tauri::command]
async fn agent_restore_project(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<(), String> {
    state
        .registry
        .lock()
        .await
        .restore(&project)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Session / run commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn agent_send(
    state: State<'_, DesktopState>,
    project: String,
    text: String,
    references: Option<Vec<String>>,
    images: Option<Vec<String>>,
    session: Option<String>,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let prompt = compose_prompt(&text, references.as_deref().unwrap_or(&[]));
    // The user turn is structured content: the prompt text (with `@` references folded in)
    // plus any pasted images. When there are no images this is a single text part, which
    // serializes as a plain string — byte-identical to the historical shape.
    let user_content = user_content(&prompt, images.as_deref().unwrap_or(&[]));
    spawn_turn(&state, runtime, project, session, text, user_content).await
}

/// Edit the **last user message** and resend: first rewind that turn (drop the old user
/// message and every assistant / tool message after it, both in memory and on disk), then
/// start the edited text as a brand-new turn — the old reply is discarded.
///
/// "Edit and resend" is only meaningful for the last user turn (there is no per-message
/// selection in the UI), so this is the same "option A" semantic as the TUI's `/rewind`.
#[tauri::command]
async fn agent_resend(
    state: State<'_, DesktopState>,
    project: String,
    text: String,
    references: Option<Vec<String>>,
    images: Option<Vec<String>>,
    session: Option<String>,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    // Roll back the last turn before sending the edited text. Doing this in one locked
    // step (rewind + begin_turn) keeps the agent / store / view items consistent.
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
    let prompt = compose_prompt(&text, references.as_deref().unwrap_or(&[]));
    let user_content = user_content(&prompt, images.as_deref().unwrap_or(&[]));
    spawn_turn(&state, runtime, project, session, text, user_content).await
}

/// Take the session's `Agent`, register the cancel handle, and spawn the run task that
/// drives `run_stream`, feeding every `AgentEvent` back into the session.
///
/// Shared by `agent_send` (new turn) and `agent_resend` (edit + resend): both differ only
/// in whether the previous turn was rewound first; the run driving is identical.
async fn spawn_turn(
    state: &DesktopState,
    runtime: Arc<Mutex<ProjectRuntime>>,
    project: String,
    session: Option<String>,
    text: String,
    user_content: Content,
) -> Result<(), String> {
    // Start this turn: take the session's `Agent` and optimistically record the
    // **user message** into its view items (`Session::apply_event` deliberately ignores
    // `MessageAdded(User)`, leaving it to the driver to record).
    let (mut agent, memory, session_name) = {
        let mut guard = runtime.lock().await;
        let session_name = match session {
            Some(name) => name,
            None => guard
                .sessions
                .active_name()
                .map(str::to_owned)
                .ok_or_else(|| "current session is not yet persisted".to_owned())?,
        };
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
        (agent, memory, session_name)
    };

    let catalog = runtime.lock().await.factory.session_catalog.clone();
    let runtime_for_task = runtime.clone();
    // Cancel handle: same lifetime as this turn's task. `agent_cancel` takes it out and
    // signals it; removed when the task ends (normal / cancelled / error), so no sender
    // pointing at a finished task lingers.
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    let key = route_key(&project, &session_name);
    state.cancels.lock().await.insert(key.clone(), cancel_tx);
    let cancels = state.cancels.clone();

    tauri::async_runtime::spawn(async move {
        // Catch unwinding while the Agent remains owned by this supervisor. A tool panic
        // must not drop the Agent and leave the session permanently marked as running.
        let terminal_update = guard_turn(async {
            let mut cancel_rx = cancel_rx;
            // Record pending input even if cancellation happens during memory prefetch.
            let mut stream = agent.run_stream_content(user_content);
            if let Some(memory) = memory.as_ref() {
                tokio::select! {
                    biased;
                    _ = &mut cancel_rx => return Ok(cancelled_event()),
                    _ = memory.prefetch_query_vector() => {},
                }
            }
            loop {
                tokio::select! {
                    biased;
                    _ = &mut cancel_rx => return Ok(cancelled_event()),
                    event = stream.next() => {
                        let event = event.ok_or_else(|| "Agent stream ended unexpectedly; conversation preserved.".to_owned())?
                            .map_err(|error| error.to_string())?;
                        if matches!(&event, AgentEvent::Finished(_)) {
                            return Ok(event);
                        }
                        runtime_for_task.lock().await.sessions.apply_event(&session_name, Ok(event));
                    }
                }
            }
        }).await;

        // The stream is gone. Preserve partial text and close orphan tool calls on all
        // terminal paths, including a panic, before accepting the next user message.
        let title_input = matches!(&terminal_update, Ok(AgentEvent::Finished(result))
            if matches!(result.stop_reason, StopReason::Completed))
            .then(|| first_user_message(agent.messages()))
            .flatten()
            .map(|first| (agent.model_config().clone(), first));
        let messages = agent.finish_interrupted_turn();
        cancels.lock().await.remove(&key);
        {
            let mut runtime = runtime_for_task.lock().await;
            for message in messages {
                runtime.sessions.apply_event(&session_name, Ok(AgentEvent::MessageAdded(message)));
            }
            runtime.sessions.restore_agent(&session_name, agent);
            runtime.sessions.apply_event(&session_name, terminal_update);
        }
        // Naming is ancillary: it must never keep an already-finished task loading.
        if let Some((config, first)) = title_input {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                auto_title_session(config, first, &catalog, &session_name),
            ).await;
        }
    });

    Ok(())
}

fn cancelled_event() -> AgentEvent {
    AgentEvent::Finished(RunResult {
        messages: Vec::new(),
        stop_reason: StopReason::Cancelled,
        usage: Default::default(),
    })
}

async fn guard_turn(
    future: impl std::future::Future<Output = Result<AgentEvent, String>>,
) -> Result<AgentEvent, String> {
    AssertUnwindSafe(future).catch_unwind().await.unwrap_or_else(|_| {
        Err("Task crashed; conversation preserved. Check any interrupted tool's effects before continuing.".to_owned())
    })
}

/// Auto-generate a session title after the first turn; the web model just truncates the first message.
async fn auto_title_session(config: shirley_agent_sdk::ModelConfig, first: String, catalog: &Arc<dyn SessionCatalog>, name: &str) {
    let probe_catalog = catalog.clone();
    let probe_name = name.to_owned();
    let already_named = tauri::async_runtime::spawn_blocking(move || {
        probe_catalog
            .list()
            .ok()
            .and_then(|entries| entries.into_iter().find(|entry| entry.name == probe_name))
            .map(|entry| entry.label != entry.name)
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if already_named {
        return;
    }

    // A one-shot completion for the web model creates a separate conversation on the
    // DeepSeek side. Taking the title from the first message avoids a single Shirley
    // utterance producing a second session on the web side.
    let raw = if config.model == "deepseek-web" {
        first.chars().take(20).collect::<String>()
    } else {
        let Ok(agent) = Agent::builder().model_config(config).build() else { return; };
        let Ok(raw) = agent.complete(&title_prompt(&first)).await else {
            return;
        };
        raw
    };
    let Some(title) = sanitize_title(&raw) else {
        return;
    };

    let write_catalog = catalog.clone();
    let write_name = name.to_owned();
    let _ = tauri::async_runtime::spawn_blocking(move || write_catalog.rename(&write_name, &title))
        .await;
}

/// Subscribe to a session: return its view snapshot and start a per-session pump that emits subsequent events in wire format.
#[tauri::command]
async fn agent_subscribe(
    state: State<'_, DesktopState>,
    app: AppHandle,
    project: String,
    session: String,
) -> Result<SessionSnapshotWire, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let key = route_key(&project, &session);

    if let Some(handle) = state.pumps.lock().await.remove(&key) {
        handle.abort();
    }
    // **Within the same lock**, subscribe first then take the snapshot: `apply_event`
    // also takes this lock, so no event can be inserted between the two — the snapshot
    // covers before-subscribe, the channel covers after-subscribe, nothing lost or duplicated.
    let (mut rx, items, running, todo) = {
        let guard = runtime.lock().await;
        let rx = guard
            .sessions
            .subscribe(&session)
            .ok_or_else(|| format!("session not found: {session}"))?;
        let items = guard
            .sessions
            .snapshot(&session)
            .ok_or_else(|| format!("session not found: {session}"))?;
        let running = guard.sessions.is_running(&session);
        // Take the task-ledger snapshot under the same lock — the task card is visible
        // immediately when switching to the session, without waiting for the next `todo`
        // tool completion.
        let todo = guard
            .sessions
            .session(&session)
            .and_then(|session| session.todo_view());
        (rx, items, running, todo)
    };

    let pumps = state.pumps.clone();
    let pump_project = project.clone();
    let pump_session = session.clone();
    let pump_key = key.clone();
    let pump_runtime = runtime.clone();
    let handle = tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Ok(event)) => {
                    // `todo` tool completion = task-ledger change. The SDK emits no
                    // dedicated event, so beyond forwarding the tool event we read the
                    // ledger snapshot once more and send a `TodoUpdated` to the frontend —
                    // making "the task is progressing" visible to the user (the ledger does
                    // not parse the injected XML; it takes the structured view).
                    let is_todo = matches!(
                        &event,
                        AgentEvent::ToolFinished { name, .. } if name == "todo"
                    );
                    if let Some(wire) = AgentEventWire::from_event(event)
                        .map(|wire| wire.with_route(&pump_project, &pump_session))
                    {
                        let _ = app.emit(EVENT_NAME, wire);
                    }
                    if is_todo {
                        let todo = {
                            let guard = pump_runtime.lock().await;
                            guard
                                .sessions
                                .session(&pump_session)
                                .and_then(|session| session.todo_view())
                        };
                        let wire = AgentEventWire::TodoUpdated {
                            project: Some(pump_project.clone()),
                            session: Some(pump_session.clone()),
                            todo: todo.map(TaskViewWire::from),
                        };
                        let _ = app.emit(EVENT_NAME, wire);
                    }
                }
                Ok(Err(message)) => {
                    let wire = AgentEventWire::Error {
                        project: Some(pump_project.clone()),
                        session: Some(pump_session.clone()),
                        message,
                    };
                    let _ = app.emit(EVENT_NAME, wire);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        pumps.lock().await.remove(&pump_key);
    });
    let snapshot = SessionSnapshotWire::new(session.clone(), items, running, todo);
    state.pumps.lock().await.insert(key, handle);

    Ok(snapshot)
}

/// Stop a session's pump (called when switching away). Absent = silent success (idempotent).
#[tauri::command]
async fn agent_unsubscribe(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
) -> Result<(), String> {
    let key = route_key(&project, &session);
    if let Some(handle) = state.pumps.lock().await.remove(&key) {
        handle.abort();
    }
    Ok(())
}

/// Current model name (this project's foreground session).
///
/// Goes through the session-level query ([`SessionManager::active_model_name`]),
/// **usable at runtime too**: while the foreground session is running a turn its
/// `Agent` has been `take`n away, so reading `active_agent()` directly returns an empty
/// string and the frontend clears the model selector — which is exactly the "switch
/// away and back, the model does not restore" symptom.
#[tauri::command]
async fn agent_model_name(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<String, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let guard = runtime.lock().await;
    Ok(guard
        .sessions
        .active_model_name()
        .map(str::to_owned)
        .unwrap_or_default())
}

/// List the models available from the current project's endpoint.
#[tauri::command]
async fn agent_list_models(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<ModelEntryWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let catalog = runtime.lock().await.factory.model_catalog.clone();
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

/// Hot-swap the model: no Agent rebuild, only `ModelConfig::model` (same semantics as the TUI's `/model`).
///
/// Also **auto-selects the tool protocol** by model declaration
/// (`docs/tool-calling-refactor.md` Step 3): look up the entry by `model` value in the
/// model catalog, compute a full `ModelConfig` (model value + tool protocol) via
/// [`crate::bootstrap::AgentFactory::model_config_for`], then apply it in one shot
/// ([`shirley_agent_sdk::Agent::set_model_config`]). The frontend is unaware of the
/// protocol — it only passes the model value; the selection is entirely on the Rust side.
#[tauri::command]
async fn agent_set_model(
    state: State<'_, DesktopState>,
    project: String,
    model: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    // Reverse lookup in the catalog: clone out factory / catalog first, then lock sessions (avoid holding a lock across await).
    let (factory, catalog) = {
        let guard = runtime.lock().await;
        (guard.factory.clone(), guard.factory.model_catalog.clone())
    };
    let entry = catalog
        .list()
        .await
        .into_iter()
        .find(|entry| entry.value == model);
    // Compute a full `ModelConfig` (model value + tool protocol) from the model entry
    // and **apply it in one shot** (`docs/tool-calling-refactor.md` Step 3). If not found
    // in the catalog, just change the model name.
    let config = entry.as_ref().map(|entry| factory.model_config_for(entry));

    let mut guard = runtime.lock().await;
    let session = guard.sessions.active_mut();
    let agent = session
        .agent
        .as_mut()
        .ok_or_else(|| "agent is currently running".to_owned())?;
    match config {
        Some(config) => agent.set_model_config(config),
        None => agent.set_model(model),
    }
    // Sync the session's model-name mirror so the new model still shows after switching away and back at runtime.
    session.remember_model();
    Ok(())
}

/// `@` reference workspace file search (this project's workspace root).
#[tauri::command]
async fn agent_search_files(
    state: State<'_, DesktopState>,
    project: String,
    query: String,
) -> Result<Vec<FileEntryWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let root = runtime.lock().await.factory.working_dir.clone();
    let entries = tauri::async_runtime::spawn_blocking(move || {
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

/// List the skills visible to a project's current session (for `/` candidates).
///
/// The skill catalog is shared across sessions (scanned once at assembly), so we take
/// the `SkillRuntime` of any loaded session in the project; prefer the current
/// foreground session. If the project runtime is not yet loaded, `ensure_runtime` first
/// (which scans the catalog once).
#[tauri::command]
async fn agent_list_skills(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<SkillEntryWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
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

/// Activate a skill in a given session of a project (the `/<skill-name>` entry, same semantics as the TUI).
///
/// Writes only that session's `SkillRuntime`: on the next turn `SkillContextProvider`
/// appends its body (`docs/skills.md` 5.6, no new injection seam). Unknown skill /
/// read failure returns an error message.
#[tauri::command]
async fn agent_activate_skill(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
    name: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let guard = runtime.lock().await;
    let session_ref = guard
        .sessions
        .session(&session)
        .ok_or_else(|| format!("session not found: {session}"))?;
    let skills = session_ref
        .skills
        .as_ref()
        .ok_or_else(|| "the current session does not support skills".to_owned())?;
    skills
        .activate(&name)
        .map(|_| ())
        .map_err(|error| match error {
            crate::skills::runtime::ActivateError::UnknownSkill(name) => {
                format!("unknown skill: {name}")
            }
            crate::skills::runtime::ActivateError::ReadFailed(reason) => reason,
        })
}

/// List all sessions of a project (most recently modified first).
#[tauri::command]
async fn agent_list_sessions(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<SessionEntryWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let catalog = runtime.lock().await.factory.session_catalog.clone();
    let entries = tauri::async_runtime::spawn_blocking(move || catalog.list())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let guard = runtime.lock().await;
    Ok(entries
        .into_iter()
        .map(|entry| {
            let running = guard.sessions.is_running(&entry.name);
            let mut wire = SessionEntryWire::from(entry);
            wire.running = running;
            wire
        })
        .collect())
}

/// A project's current foreground session name (for frontend display / tagging).
#[tauri::command]
async fn agent_current_session(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Option<String>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    Ok(runtime
        .lock()
        .await
        .sessions
        .active_name()
        .map(str::to_owned))
}

/// Create a session, optionally named (`title` empty = anonymous). Lazily created via `SessionManager` and switched to.
#[tauri::command]
async fn agent_new_session(
    state: State<'_, DesktopState>,
    project: String,
    title: Option<String>,
) -> Result<SessionEntryWire, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let name = runtime.lock().await.sessions.create_new(title.as_deref())?;
    Ok(SessionEntryWire {
        name: name.clone(),
        label: name,
        preview: String::new(),
        modified_ms: 0,
        turns: 0,
        running: false,
    })
}

/// Switch to the given session (moves the `active` pointer via `SessionManager`).
#[tauri::command]
async fn agent_switch_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    runtime.lock().await.sessions.switch_to(&name)?;
    Ok(())
}

/// Rename a session: **changes the title only, not the identity / content**.
#[tauri::command]
async fn agent_rename_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
    title: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let catalog = runtime.lock().await.factory.session_catalog.clone();
    tauri::async_runtime::spawn_blocking(move || catalog.rename(&name, &title))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Delete a session (log and title included). Rejected while running.
#[tauri::command]
async fn agent_delete_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    runtime.lock().await.sessions.delete_session(&name)?;
    let catalog = runtime.lock().await.factory.session_catalog.clone();
    let delete_name = name.clone();
    tauri::async_runtime::spawn_blocking(move || catalog.delete(&delete_name))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Move a session to another project (cross-project session log migration).
///
/// Semantics: move the source session's `<name>.jsonl` and `<name>.title` from the
/// source project's `<root>/.shirley/sessions` to the same path under the target
/// project, and have the source `SessionManager` forget it (rejected while running).
/// **Does not auto-switch to the target session** — avoiding an unexpected jump that
/// interrupts the user.
///
/// Both source and target use the same [`SessionCatalog`] contract: `storage_dir()`
/// locates the target directory, `move_to()` performs the migration. Error if source
/// and target directories are the same (same project) — there is no move semantics.
#[tauri::command]
async fn agent_move_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
    target_project: String,
) -> Result<(), String> {
    if project == target_project {
        return Err("source and target projects are the same".to_owned());
    }
    let source = ensure_runtime(&state, &project).await?;
    let target = ensure_runtime(&state, &target_project).await?;
    // First have the source manager forget the session (rejected here while running) —
    // so that after the session file moves, the source memory doesn't still hold an
    // `Agent` / store handle pointing at the old path.
    {
        let mut guard = source.lock().await;
        guard.sessions.forget_session(&name)?;
    }
    // Target directory: the target project's session directory (same `SessionCatalog` as the source).
    let dest_dir = target
        .lock()
        .await
        .factory
        .session_catalog
        .storage_dir()
        .ok_or_else(|| "the target project does not support a session directory".to_owned())?;
    let catalog = source.lock().await.factory.session_catalog.clone();
    let move_name = name.clone();
    tauri::async_runtime::spawn_blocking(move || catalog.move_to(&move_name, &dest_dir))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Replay a project's current session history.
#[tauri::command]
async fn agent_load_history(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<HistoryMessageWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let guard = runtime.lock().await;
    let agent = guard
        .sessions
        .active_agent()
        .ok_or_else(|| "agent is currently running".to_owned())?;
    Ok(history_from_messages(agent.messages()))
}

/// A subagent summary replayed to the frontend sidebar (`agent_list_subagents`), aligned with
/// `SubagentSummaryWire` in `web/src/lib/bridge.ts`. `summary` / `stop_reason` appear only once the
/// subagent has reached a terminal state.
#[derive(Debug, Clone, Serialize)]
struct SubagentSummaryWire {
    id: String,
    kind: &'static str,
    task: String,
    status: &'static str,
    started_ms: u64,
    finished_ms: Option<u64>,
    elapsed_ms: u64,
    summary: String,
    stop_reason: String,
}

impl SubagentSummaryWire {
    fn from_info(info: &SubagentInfo) -> Self {
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

/// The subagent manager of a given session in a project (`None` = the session is not loaded, or the
/// session has no subagents). Reads the in-memory registry, which is seeded from `index.jsonl` when
/// the session's manager is created (`SubagentManager::set_store`).
async fn subagent_manager(
    state: &DesktopState,
    project: &str,
    session: &str,
) -> Result<Arc<SubagentManager>, String> {
    let runtime = ensure_runtime(state, project).await?;
    let guard = runtime.lock().await;
    guard
        .sessions
        .session(session)
        .and_then(|session| session.subagents.clone())
        .ok_or_else(|| format!("session has no subagents: {session}"))
}

/// List a session's subagents (from the in-memory registry, seeded from `index.jsonl`).
#[tauri::command]
async fn agent_list_subagents(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
) -> Result<Vec<SubagentSummaryWire>, String> {
    let manager = subagent_manager(&state, &project, &session).await?;
    Ok(manager
        .list()
        .iter()
        .map(SubagentSummaryWire::from_info)
        .collect())
}

/// Replay a subagent's transcript (`<id>.jsonl`) as `HistoryMessageWire[]` (same style as
/// `agent_load_history`). Empty when the subagent has no transcript yet (still running, no store).
#[tauri::command]
async fn agent_load_subagent(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
    id: String,
) -> Result<Vec<HistoryMessageWire>, String> {
    let manager = subagent_manager(&state, &project, &session).await?;
    Ok(history_from_messages(&manager.load_transcript(&id)))
}

/// Cancel a running subagent (`AbortHandle::abort`, decision 7). Idempotent: cancelling an already
/// finished / unknown subagent is a silent no-op.
#[tauri::command]
async fn agent_cancel_subagent(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
    id: String,
) -> Result<(), String> {
    let manager = subagent_manager(&state, &project, &session).await?;
    // An unknown id is treated as a silent no-op (the sidebar may race a just-finished subagent).
    let _ = manager.cancel(&id);
    Ok(())
}

/// Cancel the running turn of the given session.
///
/// Take out the cancel handle registered by `agent_send` for that session and signal
/// it; the task driving `run_stream` drops the stream, rolls back this turn's Agent
/// state, and finishes with `Finished(Cancelled)`. Silent success (idempotent) when the
/// session is not running (handle removed / never started).
#[tauri::command]
async fn agent_cancel(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
) -> Result<(), String> {
    let key = route_key(&project, &session);
    if let Some(cancel) = state.cancels.lock().await.remove(&key) {
        // `send` failure = the task already ended (receiver dropped); harmless.
        let _ = cancel.send(());
    }
    Ok(())
}

pub fn run(factory: AgentFactory) -> std::io::Result<()> {
    let factory = Arc::new(factory);
    let working_dir = factory.working_dir.clone();

    // Register the current workspace as the default project (idempotent: only refreshes the open time if already registered).
    let mut registry =
        ProjectRegistry::load_default().map_err(|e| std::io::Error::other(e.to_string()))?;
    let default_entry = registry
        .add(working_dir, None)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    eprintln!(
        "[desktop] default project: {} ({})",
        default_entry.name, default_entry.id
    );

    let runtime = runtime_from_factory(factory).map_err(std::io::Error::other)?;
    let mut runtimes = HashMap::new();
    runtimes.insert(default_entry.id.clone(), Arc::new(Mutex::new(runtime)));

    let state = DesktopState {
        registry: Arc::new(Mutex::new(registry)),
        runtimes: Arc::new(Mutex::new(runtimes)),
        pumps: Arc::new(Mutex::new(HashMap::new())),
        cancels: Arc::new(Mutex::new(HashMap::new())),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            agent_list_projects,
            agent_add_project,
            agent_rename_project,
            agent_archive_project,
            agent_restore_project,
            agent_send,
            agent_resend,
            agent_subscribe,
            agent_unsubscribe,
            agent_cancel,
            agent_model_name,
            agent_list_models,
            agent_set_model,
            agent_search_files,
            agent_list_skills,
            agent_activate_skill,
            agent_list_sessions,
            agent_current_session,
            agent_new_session,
            agent_switch_session,
            agent_rename_session,
            agent_delete_session,
            agent_move_session,
            agent_load_history,
            agent_list_subagents,
            agent_load_subagent,
            agent_cancel_subagent
        ])
        .run(tauri::generate_context!())
        .map_err(|error| std::io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::FileSessionCatalog;

    #[tokio::test]
    async fn task_panic_keeps_the_agent_and_user_input_available() {
        struct PanickingContext;
        impl shirley_agent_sdk::ContextProvider for PanickingContext {
            fn context(&self) -> Option<String> { panic!("injected task panic") }
        }
        let mut agent = Agent::builder()
            .model_config(shirley_agent_sdk::ModelConfig::builder()
                .protocol(shirley_agent_sdk::ModelProtocol::ChatCompletions)
                .base_url("http://127.0.0.1:1")
                .model("fixture").build())
            .context_provider(Arc::new(PanickingContext) as Arc<dyn shirley_agent_sdk::ContextProvider>)
            .build().unwrap();
        let result = guard_turn(async {
            let mut stream = agent.run_stream("keep this input");
            while let Some(event) = stream.next().await {
                event.map_err(|error| error.to_string())?;
            }
            Ok(cancelled_event())
        }).await;
        assert!(result.unwrap_err().contains("Task crashed"));
        agent.finish_interrupted_turn();
        assert!(matches!(agent.messages(), [Message::User { content }] if content == "keep this input"));
        assert!(agent.finish_interrupted_turn().is_empty());
    }

    fn tmp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shirley_shell_{tag}_{}", std::process::id()))
    }

    /// Regression: lazily loading a project with an **existing session** must resume
    /// its most recently modified session, not conjure an empty one — otherwise the
    /// frontend switching projects would turn the selected session into new session.
    #[test]
    fn runtime_from_factory_resumes_latest_session() {
        let root = tmp_root("resume_latest");
        let _ = std::fs::remove_dir_all(&root);

        let catalog = FileSessionCatalog::new(&root);
        let (older, older_store) = catalog.create_named(Some("older session")).unwrap();
        older_store
            .append(&Message::User {
                content: "old".into(),
            })
            .unwrap();
        // Separate the mtimes so the "most recently modified" check doesn't jitter within the same millisecond.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (newer, newer_store) = catalog.create_named(Some("newer session")).unwrap();
        newer_store
            .append(&Message::User {
                content: "new".into(),
            })
            .unwrap();

        let factory = Arc::new(AgentFactory::assemble(root.clone()).unwrap());
        let runtime = runtime_from_factory(factory).unwrap();

        assert_eq!(
            runtime.sessions.active_name(),
            Some(newer.name.as_str()),
            "should resume the most recently modified session, not create an empty one"
        );
        assert_ne!(runtime.sessions.active_name(), Some(older.name.as_str()));
        // The restored session's view items should already be rebuilt (with history), otherwise switching back shows a blank view.
        let snapshot = runtime.sessions.snapshot(&newer.name).unwrap();
        assert!(!snapshot.is_empty(), "a resumed session should carry history items");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Only lazily open an empty session when the project has **no sessions at all** (naming only, without persisting).
    #[test]
    fn runtime_from_factory_creates_empty_session_when_none_exists() {
        let root = tmp_root("resume_empty");
        let _ = std::fs::remove_dir_all(&root);

        let factory = Arc::new(AgentFactory::assemble(root.clone()).unwrap());
        let runtime = runtime_from_factory(factory).unwrap();

        assert!(runtime.sessions.active_name().is_some(), "an empty session should be opened");
        // The lazy session is not persisted yet: there should be no jsonl in the session directory.
        assert!(
            FileSessionCatalog::new(&root).list().unwrap().is_empty(),
            "a lazy empty session should not be persisted immediately"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
