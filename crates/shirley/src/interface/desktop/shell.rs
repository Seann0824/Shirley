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

use super::browser::BrowserState;
use super::wire::{AgentEventWire, SessionSnapshotWire, StateSnapshotWire, TaskViewWire};
use crate::interface::runtime::{
    FileEntryWire, HistoryMessageWire, ModelEntryWire, ProjectEntryWire, ProjectRuntimeManager,
    SessionEntryWire, SkillEntryWire, SubagentSummaryWire, route_key,
};
use crate::project::ProjectRegistry;
use shirley_agent_sdk::AgentEvent;

/// The single event name the webview listens on.
/// Custom URL scheme the webview loads artifacts from. The single path segment is an **opaque
/// handle** minted by [`agent_asset_url`], never a filesystem path (`docs/render-parts.md` P1).
const ASSET_SCHEME: &str = "shirley-asset";

const EVENT_NAME: &str = "agent://event";

/// Content-free runtime-status channel (separate from `agent://event`, see `docs/agent-status.md`).
const STATUS_EVENT_NAME: &str = "status://event";

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
async fn agent_model_providers(
    action: String,
    input: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    crate::model_providers::command(
        &crate::storage::global_root(),
        &action,
        input.unwrap_or_default(),
    )
    .await
}

#[tauri::command]
async fn agent_remote_info() -> Result<crate::remote::RemoteInfo, String> {
    crate::remote::info(&crate::storage::global_root())
}

#[tauri::command]
async fn agent_remote_save(
    state: State<'_, DesktopState>,
    url: String,
    token: Option<String>,
    sync_personal_memory: bool,
) -> Result<(), String> {
    let global = crate::storage::global_root();
    let previous = crate::remote::RemoteConfig::load(&global)?;
    let url = url.trim().trim_end_matches('/').to_string();
    let destination_changed = previous.url != url;
    // Never silently reuse a credential for a different host.
    let token = token
        .filter(|s| !s.trim().is_empty())
        .or_else(|| (previous.url == url).then_some(previous.token))
        .unwrap_or_default();
    let remote = crate::remote::RemoteConfig {
        url,
        token,
        sync_personal_memory,
    };
    // Saving a new destination requires a successful connection, before any memories are sent.
    if !remote.url.is_empty() {
        remote
            .command("agent_list_projects", serde_json::json!({}))
            .await?;
    }
    remote.save(&global)?;
    if destination_changed {
        // Reset stale sync counters, but write a well-formed status: a bare `{}` is not a
        // valid `SyncStatus` and would make the next `remote::info` fail to deserialize.
        let reset =
            serde_json::to_vec(&crate::remote::SyncStatus::default()).map_err(|e| e.to_string())?;
        crate::memory::store::atomic_write(&global.join("remote-sync-status.json"), &reset)
            .map_err(|e| e.to_string())?;
    }
    if let Some(pump) = state.remote_pump.lock().await.take() {
        pump.abort();
    }
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
        crate::remote::RemoteConfig::load(&global)?
            .post("api/memory/review", &serde_json::json!({}))
            .await
    } else {
        let records = crate::memory::sync::review_legacy(&global.join("memory"))
            .map_err(|e| e.to_string())?;
        serde_json::to_value(records).map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn agent_memory_confirm_personal(
    remote: bool,
    approvals: Vec<crate::memory::sync::MemoryApproval>,
) -> Result<(), String> {
    let global = crate::storage::global_root();
    if remote {
        crate::remote::RemoteConfig::load(&global)?
            .post(
                "api/memory/confirm-personal",
                &serde_json::json!({"approvals": approvals}),
            )
            .await?;
        Ok(())
    } else {
        crate::memory::sync::confirm_personal(&global.join("memory"), &approvals)
            .map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn agent_remote_call(
    command: String,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let remote = crate::remote::RemoteConfig::load(&crate::storage::global_root())?;
    remote.command(&command, args).await
}

async fn connect_remote_events(
    remote: &crate::remote::RemoteConfig,
) -> Result<reqwest::Response, String> {
    let request = remote.client(true)?.get(remote.endpoint("api/events"));
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        remote.authorize(request).send(),
    )
    .await
    .map_err(|_| "云端事件连接超时".to_string())?
    .map_err(|e| e.without_url().to_string())?;
    if !response.status().is_success() {
        return Err(format!("云端事件连接失败 ({})", response.status()));
    }
    Ok(response)
}

#[tauri::command]
async fn agent_remote_start_events(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<(), String> {
    let mut pump = state.remote_pump.lock().await;
    if pump.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return Ok(());
    }
    let remote = crate::remote::RemoteConfig::load(&crate::storage::global_root())?;
    let initial = connect_remote_events(&remote).await?;
    *pump = Some(tokio::spawn(async move {
        let mut response = initial;
        loop {
            let mut decoder = crate::remote::EventDecoder::default();
            loop {
                let chunk =
                    tokio::time::timeout(std::time::Duration::from_secs(45), response.chunk())
                        .await;
                let bytes = match chunk {
                    Ok(Ok(Some(bytes))) => bytes,
                    _ => break,
                };
                match decoder.push(&bytes) {
                    Ok(events) => {
                        for event in events {
                            if event.get("type").and_then(|v| v.as_str())
                                == Some("transport_resync")
                            {
                                let _ = app.emit("agent://remote-connection", true);
                                continue;
                            }
                            let _ = app.emit("agent://remote-event", event);
                        }
                    }
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

/// Edit a project: rename and/or replace its source folders (the "Edit project" dialog).
///
/// `folders` is a whole replacement; `primary` (when given) must be one of them. Changing the
/// primary folder rebuilds the project runtime (sessions live under the primary's storage key).
#[tauri::command]
async fn agent_update_project(
    state: State<'_, DesktopState>,
    project: String,
    name: Option<String>,
    primary: Option<String>,
    folders: Option<Vec<String>>,
) -> Result<ProjectEntryWire, String> {
    let primary = primary.map(PathBuf::from);
    let folders = folders.map(|list| list.into_iter().map(PathBuf::from).collect());
    state
        .manager
        .update_project(&project, name, primary, folders)
        .await
}

/// Open the native folder picker and return the chosen absolute path **without registering it**.
/// Used by the "Add folder" button inside the Edit-project dialog, which accumulates folders before
/// saving them all at once via `agent_update_project`.
/// What an ingest command hands back: the entries that made it, plus one line per failure.
#[derive(Debug, Clone, serde::Serialize)]
struct IngestWire {
    entries: Vec<crate::artifact::LibraryEntry>,
    errors: Vec<String>,
}

#[tauri::command]
async fn agent_pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| e.to_string())?;
    Ok(picked
        .and_then(|file| file.into_path().ok())
        .map(|path| path.to_string_lossy().into_owned()))
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
        // Status heartbeat: poll the session's own `StateKeeper` every second and emit the
        // content-free snapshot on the separate `status://event` channel. It runs *alongside*
        // `rx.recv()` because status must tick even when no `AgentEvent` arrives — that is the
        // whole point of stall detection.
        let mut hb = tokio::time::interval(std::time::Duration::from_secs(1));
        hb.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
              received = rx.recv() => {
                match received {
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
              _ = hb.tick() => {
                  let snapshot = match manager.ensure_runtime(&pump_project).await {
                      Ok(runtime) => {
                          let guard = runtime.lock().await;
                          guard.sessions.session(&pump_session).map(|s| s.status.snapshot())
                      }
                      Err(_) => None,
                  };
                  if let Some(snapshot) = snapshot {
                      let wire = StateSnapshotWire::from_snapshot(&snapshot)
                          .with_route(&pump_project, &pump_session);
                      let _ = app.emit(STATUS_EVENT_NAME, wire);
                  }
              }
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
    state
        .manager
        .set_model(&project, &model, session.as_deref())
        .await
}

/// Turn an artifact id into a URL the webview can load (`docs/render-parts.md` P1).
///
/// The id is validated against the project's artifact jail here; what comes back is an
/// unguessable handle, so the render layer never learns a filesystem path and a stale/forged
/// handle simply 404s.
#[tauri::command]
async fn agent_asset_url(
    state: State<'_, DesktopState>,
    project: String,
    id: String,
) -> Result<String, String> {
    let handle = state.manager.asset_handle(&project, &id).await?;
    Ok(format!("{ASSET_SCHEME}://localhost/{handle}"))
}

/// Take **local file paths** into the project's media library: the desktop inbound channel.
///
/// The desktop and the workspace are the same machine, so a path is the honest way in -- no
/// base64 through the IPC bridge, no 33% inflation, and a 400MB clip is copied by the kernel
/// instead of being carried through the webview. Returns the library entries that landed plus
/// one message per rejected path (a batch should not fail as a whole because one file was a
/// `.svg`). See `docs/render-parts.md` inbound channels.
#[tauri::command]
async fn agent_ingest_paths(
    state: State<'_, DesktopState>,
    project: String,
    paths: Vec<String>,
) -> Result<IngestWire, String> {
    let (entries, errors) = state.manager.ingest_paths(&project, paths).await?;
    Ok(IngestWire { entries, errors })
}

/// Take **raw bytes** into the project's media library over IPC.
///
/// The desktop normally uses [`agent_ingest_paths`]; this exists for the cases where the page
/// already holds the bytes (a drag from a browser window, a paste that is not a file) and for
/// parity with the cloud's `/api/upload`.
#[tauri::command]
async fn agent_ingest_bytes(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
    mime: String,
    bytes: Vec<u8>,
) -> Result<crate::artifact::LibraryEntry, String> {
    state
        .manager
        .ingest_bytes(&project, &name, &mime, &bytes)
        .await
}

/// List a project's media library, newest first (`docs/render-parts.md` inbound channels).
///
/// The library is the user's own inventory of handed-over media, not a view of one session, so
/// this reads the same `ArtifactStore` the ingest commands write to. Entries carry an opaque id;
/// the page turns that into a thumbnail URL through `agent_asset_url`, exactly like a part does.
#[tauri::command]
async fn agent_library_list(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<crate::artifact::LibraryEntry>, String> {
    state.manager.library_list(&project).await
}

/// Fetch an artifact as **text** so the page can run it (`html` artifacts, `docs/render-parts.md`).
///
/// A URL cannot serve this: the frame must receive the source string and inject it with `srcdoc`
/// on an opaque origin, otherwise generated markup would run with the app's origin.
#[tauri::command]
async fn agent_asset_text(
    state: State<'_, DesktopState>,
    project: String,
    id: String,
) -> Result<crate::interface::runtime::AssetText, String> {
    state.manager.asset_text(&project, &id).await
}

/// Serve one artifact to the webview. Deliberately local-only: no network, no redirects, and the
/// only thing a handle can reach is a file the jail already resolved.
fn asset_response<R: tauri::Runtime>(
    app: &AppHandle<R>,
    path: &str,
) -> tauri::http::Response<Vec<u8>> {
    use tauri::Manager as _;
    let handle = path.trim_start_matches('/');
    let state = app.state::<DesktopState>();
    let Some((file, mime)) = state.manager.asset_by_handle(handle) else {
        return asset_error(404, "unknown asset handle");
    };
    match std::fs::read(&file) {
        Ok(bytes) => tauri::http::Response::builder()
            .status(200)
            .header("content-type", mime)
            .header("cache-control", "no-store")
            .body(bytes)
            .unwrap_or_else(|_| tauri::http::Response::new(Vec::new())),
        Err(error) => asset_error(404, &error.to_string()),
    }
}

fn asset_error(status: u16, message: &str) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(message.as_bytes().to_vec())
        .unwrap_or_else(|_| tauri::http::Response::new(Vec::new()))
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
    state
        .manager
        .activate_skill(&project, &session, &name)
        .await
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
    state
        .manager
        .switch_session(&project, &name)
        .await
        .map(|_| ())
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
async fn agent_asr_capabilities() -> Result<crate::asr::AsrCapabilities, String> {
    // Read on every call: installing whisper.cpp takes effect without restarting the app.
    // Deliberately not project-scoped -- dictation is a property of *this computer*, and the
    // audio is transcribed here, never on a server.
    Ok(crate::asr::capabilities(&crate::storage::global_root()))
}

#[tauri::command]
async fn agent_transcribe(
    audio: String,
    language: Option<String>,
) -> Result<crate::asr::AsrResult, String> {
    crate::asr::transcribe(&crate::storage::global_root(), &audio, language.as_deref()).await
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
    tauri::async_runtime::block_on(
        async move { manager_for_init.add_project(working_dir, None).await },
    )
    .map_err(std::io::Error::other)?;

    let state = DesktopState {
        manager,
        pumps: Arc::new(Mutex::new(HashMap::new())),
        remote_pump: Mutex::new(None),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_uri_scheme_protocol(ASSET_SCHEME, |ctx, request| {
            asset_response(ctx.app_handle(), request.uri().path())
        })
        .manage(state)
        .manage(BrowserState::default())
        .invoke_handler(tauri::generate_handler![
            agent_model_providers,
            agent_remote_info,
            agent_remote_save,
            agent_remote_sync,
            agent_remote_call,
            agent_remote_start_events,
            agent_memory_review,
            agent_memory_confirm_personal,
            agent_asr_capabilities,
            agent_transcribe,
            agent_list_projects,
            agent_add_project,
            agent_rename_project,
            agent_archive_project,
            agent_restore_project,
            agent_update_project,
            agent_pick_folder,
            agent_send,
            agent_resend,
            agent_subscribe,
            agent_unsubscribe,
            agent_cancel,
            agent_model_name,
            agent_list_models,
            agent_set_model,
            agent_search_files,
            agent_asset_url,
            agent_asset_text,
            agent_ingest_paths,
            agent_ingest_bytes,
            agent_library_list,
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
            agent_cancel_subagent,
            super::browser::agent_browser_create,
            super::browser::agent_browser_set_bounds,
            super::browser::agent_browser_navigate,
            super::browser::agent_browser_back,
            super::browser::agent_browser_forward,
            super::browser::agent_browser_reload,
            super::browser::agent_browser_activate,
            super::browser::agent_browser_close,
            super::browser::agent_browser_destroy_all,
            super::browser::agent_browser_hide_all
        ])
        .run(tauri::generate_context!())
        .map_err(|error| std::io::Error::other(error.to_string()))
}
