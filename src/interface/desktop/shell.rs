//! Tauri 2 shell: a **thin transport** over the shared multi-project runtime core
//! ([`crate::interface::runtime::ProjectRuntimeManager`]).
//!
//! All orchestration (project registry, lazily-loaded per-project runtimes, the turn
//! driver, every query command and the wire DTOs) lives in the interface-agnostic core;
//! this module only adapts it to Tauri:
//!
//! - it owns a [`ProjectRuntimeManager`] whose event `sink` is a **no-op** — desktop
//!   events reach the webview through each session's own `Session::publish` channel,
//!   drained by a per-`project::session` pump ([`agent_subscribe`]) that re-emits them
//!   as `agent://event` wire messages.
//! - each `#[tauri::command]` is a one-line delegate to a manager method; the only
//!   transport-specific command is `agent_add_project` (it opens the **native folder
//!   picker** first, then hands the path to `manager.add_project`).
//!
//! See `docs/cloud-server.md` for the design rationale (the HTTP/SSE `server.rs` is the
//! sibling transport over the exact same core).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;

use super::wire::{AgentEventWire, SessionSnapshotWire, TaskViewWire};
use crate::interface::runtime::{
    FileEntryWire, HistoryMessageWire, ModelEntryWire, ProjectEntryWire, ProjectRuntimeManager,
    SessionEntryWire, SkillEntryWire, SubagentSummaryWire, route_key,
};
use crate::project::ProjectRegistry;
use shirley_agent_sdk::AgentEvent;

/// The single event name the webview listens on.
const EVENT_NAME: &str = "agent://event";

/// Chat mode workspace root: `~/.shirley/chat`.
fn chat_root() -> PathBuf {
    crate::storage::global_root().join("chat")
}

/// Tauri-managed runtime state.
///
/// - `manager`: the shared orchestration core (registry + lazily-loaded runtimes +
///   turn driver + query commands). Its `sink` is a no-op on desktop.
/// - `pumps`: the per-`project::session` event pump handle, drained from the session's
///   own broadcast channel.
struct DesktopState {
    manager: Arc<ProjectRuntimeManager>,
    pumps: Arc<Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>>,
    remote_pump: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[tauri::command]
async fn agent_model_providers(action: String, input: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    crate::model_providers::command(&crate::storage::global_root(), &action, input.unwrap_or_default()).await
}

#[tauri::command]
async fn agent_remote_info() -> Result<crate::remote::RemoteInfo, String> {
    crate::remote::info(&crate::storage::global_root())
}

#[tauri::command]
async fn agent_remote_save(
    state: State<'_, DesktopState>, url: String, token: Option<String>, sync_personal_memory: bool,
) -> Result<(), String> {
    let global = crate::storage::global_root();
    let previous = crate::remote::RemoteConfig::load(&global)?;
    let url = url.trim().trim_end_matches('/').to_string();
    let destination_changed = previous.url != url;
    // Never silently reuse a credential for a different host.
    let token = token.filter(|s| !s.trim().is_empty())
        .or_else(|| (previous.url == url).then_some(previous.token)).unwrap_or_default();
    let remote = crate::remote::RemoteConfig { url, token, sync_personal_memory };
    // Saving a new destination requires a successful connection, before any memories are sent.
    if !remote.url.is_empty() {
        remote.command("agent_list_projects", serde_json::json!({})).await?;
    }
    remote.save(&global)?;
    if destination_changed {
        // Reset stale sync counters, but write a well-formed status: a bare `{}` is not a
        // valid `SyncStatus` and would make the next `remote::info` fail to deserialize.
        let reset = serde_json::to_vec(&crate::remote::SyncStatus::default())
            .map_err(|e| e.to_string())?;
        crate::memory::store::atomic_write(&global.join("remote-sync-status.json"), &reset)
            .map_err(|e| e.to_string())?;
    }
    if let Some(pump) = state.remote_pump.lock().await.take() { pump.abort(); }
    Ok(())
}

#[tauri::command]
async fn agent_remote_sync() -> Result<crate::remote::SyncStatus, String> {
    crate::remote::sync_once(&crate::storage::global_root()).await
}

#[tauri::command]
async fn agent_memory_review(remote: bool) -> Result<serde_json::Value, String> {
    let global = crate::storage::global_root();
    if remote {
        crate::remote::RemoteConfig::load(&global)?.post("api/memory/review", &serde_json::json!({})).await
    } else {
        let records = crate::memory::sync::review_legacy(&global.join("memory")).map_err(|e| e.to_string())?;
        serde_json::to_value(records).map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn agent_memory_confirm_personal(remote: bool, approvals: Vec<crate::memory::sync::MemoryApproval>) -> Result<(), String> {
    let global = crate::storage::global_root();
    if remote {
        crate::remote::RemoteConfig::load(&global)?.post("api/memory/confirm-personal", &serde_json::json!({"approvals": approvals})).await?;
        Ok(())
    } else {
        crate::memory::sync::confirm_personal(&global.join("memory"), &approvals).map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn agent_remote_call(command: String, args: serde_json::Value) -> Result<serde_json::Value, String> {
    let remote = crate::remote::RemoteConfig::load(&crate::storage::global_root())?;
    remote.command(&command, args).await
}

async fn connect_remote_events(remote: &crate::remote::RemoteConfig) -> Result<reqwest::Response, String> {
    let request = remote.client(true)?.get(remote.endpoint("api/events"));
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), remote.authorize(request).send())
        .await.map_err(|_| "云端事件连接超时".to_string())?
        .map_err(|e| e.without_url().to_string())?;
    if !response.status().is_success() { return Err(format!("云端事件连接失败 ({})", response.status())); }
    Ok(response)
}

#[tauri::command]
async fn agent_remote_start_events(app: AppHandle, state: State<'_, DesktopState>) -> Result<(), String> {
    let mut pump = state.remote_pump.lock().await;
    if pump.as_ref().is_some_and(|handle| !handle.is_finished()) { return Ok(()); }
    let remote = crate::remote::RemoteConfig::load(&crate::storage::global_root())?;
    let initial = connect_remote_events(&remote).await?;
    *pump = Some(tokio::spawn(async move {
        let mut response = initial;
        loop {
            let mut decoder = crate::remote::EventDecoder::default();
            loop {
                let chunk = tokio::time::timeout(std::time::Duration::from_secs(45), response.chunk()).await;
                let bytes = match chunk { Ok(Ok(Some(bytes))) => bytes, _ => break };
                match decoder.push(&bytes) {
                    Ok(events) => for event in events {
                        if event.get("type").and_then(|v| v.as_str()) == Some("transport_resync") {
                            let _ = app.emit("agent://remote-connection", true);
                            continue;
                        }
                        let _ = app.emit("agent://remote-event", event);
                    },
                    Err(_) => break,
                }
            }
            let _ = app.emit("agent://remote-connection", false);
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if let Ok(next) = connect_remote_events(&remote).await {
                    response = next;
                    let _ = app.emit("agent://remote-connection", true);
                    break;
                }
            }
        }
    }));
    Ok(())
}

// ---------------------------------------------------------------------------
// Project commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn agent_list_projects(
    state: State<'_, DesktopState>,
    archived: Option<bool>,
) -> Result<Vec<ProjectEntryWire>, String> {
    Ok(state.manager.list_projects(archived.unwrap_or(false)).await)
}

/// Open the native folder picker, then register + eagerly build the project runtime.
#[tauri::command]
async fn agent_add_project(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Option<ProjectEntryWire>, String> {
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|file| file.into_path().ok()) else {
        return Ok(None);
    };
    Ok(Some(state.manager.add_project(path, None).await?))
}

#[tauri::command]
async fn agent_rename_project(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
) -> Result<(), String> {
    state.manager.rename_project(&project, &name).await
}

#[tauri::command]
async fn agent_archive_project(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<(), String> {
    state.manager.archive_project(&project).await
}

#[tauri::command]
async fn agent_restore_project(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<(), String> {
    state.manager.restore_project(&project).await
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
    state
        .manager
        .send(
            &project,
            session,
            text,
            references.unwrap_or_default(),
            images.unwrap_or_default(),
        )
        .await
}

#[tauri::command]
async fn agent_resend(
    state: State<'_, DesktopState>,
    project: String,
    text: String,
    references: Option<Vec<String>>,
    images: Option<Vec<String>>,
    session: Option<String>,
) -> Result<(), String> {
    state
        .manager
        .resend(
            &project,
            session,
            text,
            references.unwrap_or_default(),
            images.unwrap_or_default(),
        )
        .await
}

/// Subscribe to a session: return its view snapshot and start a per-session pump that
/// emits subsequent events in wire format.
#[tauri::command]
async fn agent_subscribe(
    state: State<'_, DesktopState>,
    app: AppHandle,
    project: String,
    session: String,
) -> Result<SessionSnapshotWire, String> {
    let key = route_key(&project, &session);
    if let Some(handle) = state.pumps.lock().await.remove(&key) {
        handle.abort();
    }
    // Subscribe + snapshot happen under one lock inside the core, so nothing is lost
    // or duplicated across the snapshot / channel boundary.
    let (mut rx, snapshot) = state.manager.subscribe(&project, &session).await?;

    let pumps = state.pumps.clone();
    let manager = state.manager.clone();
    let pump_project = project.clone();
    let pump_session = session.clone();
    let pump_key = key.clone();
    let handle = tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Ok(event)) => {
                    // `todo` tool completion = task-ledger change: the SDK emits no
                    // dedicated event, so re-read the ledger snapshot and push a
                    // `TodoUpdated` after the tool event.
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
                        let todo = match manager.ensure_runtime(&pump_project).await {
                            Ok(runtime) => {
                                let guard = runtime.lock().await;
                                guard
                                    .sessions
                                    .session(&pump_session)
                                    .and_then(|session| session.todo_view())
                            }
                            Err(_) => None,
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
    state.pumps.lock().await.insert(key, handle);

    Ok(snapshot)
}

/// Stop a session's pump (called when switching away). Absent = silent success.
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

#[tauri::command]
async fn agent_cancel(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
) -> Result<(), String> {
    state.manager.cancel(&project, &session).await;
    Ok(())
}

#[tauri::command]
async fn agent_model_name(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<String, String> {
    state.manager.model_name(&project).await
}

#[tauri::command]
async fn agent_list_models(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<ModelEntryWire>, String> {
    state.manager.list_models(&project).await
}

#[tauri::command]
async fn agent_set_model(
    state: State<'_, DesktopState>,
    project: String,
    model: String,
    session: Option<String>,
) -> Result<(), String> {
    state.manager.set_model(&project, &model, session.as_deref()).await
}

#[tauri::command]
async fn agent_search_files(
    state: State<'_, DesktopState>,
    project: String,
    query: String,
) -> Result<Vec<FileEntryWire>, String> {
    state.manager.search_files(&project, query).await
}

#[tauri::command]
async fn agent_list_skills(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<SkillEntryWire>, String> {
    state.manager.list_skills(&project).await
}

#[tauri::command]
async fn agent_activate_skill(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
    name: String,
) -> Result<(), String> {
    state.manager.activate_skill(&project, &session, &name).await
}

#[tauri::command]
async fn agent_list_sessions(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<SessionEntryWire>, String> {
    state.manager.list_sessions(&project).await
}

#[tauri::command]
async fn agent_current_session(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Option<String>, String> {
    Ok(state.manager.current_session(&project).await.ok())
}

#[tauri::command]
async fn agent_new_session(
    state: State<'_, DesktopState>,
    project: String,
    title: Option<String>,
) -> Result<SessionEntryWire, String> {
    let name = state.manager.new_session(&project, title).await?;
    Ok(SessionEntryWire {
        name: name.clone(),
        label: name,
        preview: String::new(),
        modified_ms: 0,
        turns: 0,
        running: false,
    })
}

#[tauri::command]
async fn agent_switch_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
) -> Result<(), String> {
    state.manager.switch_session(&project, &name).await.map(|_| ())
}

#[tauri::command]
async fn agent_rename_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
    title: String,
) -> Result<(), String> {
    state.manager.rename_session(&project, &name, &title).await
}

#[tauri::command]
async fn agent_delete_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
) -> Result<(), String> {
    state.manager.delete_session(&project, &name).await
}

#[tauri::command]
async fn agent_move_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
    target_project: String,
) -> Result<(), String> {
    state
        .manager
        .move_session(&project, &name, &target_project)
        .await
}

#[tauri::command]
async fn agent_load_history(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<HistoryMessageWire>, String> {
    state.manager.load_history(&project).await
}

#[tauri::command]
async fn agent_list_subagents(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
) -> Result<Vec<SubagentSummaryWire>, String> {
    state.manager.list_subagents(&project, &session).await
}

#[tauri::command]
async fn agent_load_subagent(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
    id: String,
) -> Result<Vec<HistoryMessageWire>, String> {
    state.manager.load_subagent(&project, &session, &id).await
}

#[tauri::command]
async fn agent_cancel_subagent(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
    id: String,
) -> Result<(), String> {
    state.manager.cancel_subagent(&project, &session, &id).await
}

pub fn run(factory: crate::bootstrap::AgentFactory) -> std::io::Result<()> {
    let working_dir = factory.working_dir.clone();

    let registry =
        ProjectRegistry::load_default().map_err(|e| std::io::Error::other(e.to_string()))?;
    // Desktop sink is a no-op: events reach the webview through the per-session pumps.
    let manager = Arc::new(ProjectRuntimeManager::new(
        Arc::new(Mutex::new(registry)),
        Some(chat_root()),
        Arc::new(|_| {}),
    ));

    // Register + eagerly build the current workspace as the default project.
    let manager_for_init = manager.clone();
    tauri::async_runtime::block_on(async move {
        manager_for_init.add_project(working_dir, None).await
    })
    .map_err(std::io::Error::other)?;

    let state = DesktopState {
        manager,
        pumps: Arc::new(Mutex::new(HashMap::new())),
        remote_pump: Mutex::new(None),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            agent_model_providers,
            agent_remote_info,
            agent_remote_save,
            agent_remote_sync,
            agent_remote_call,
            agent_remote_start_events,
            agent_memory_review,
            agent_memory_confirm_personal,
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
