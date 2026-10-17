//! Cloud web interface: an HTTP + SSE server that runs the Agent **on the server
//! side**, keeping the full tool set (bash / read / grep / todo / memory / skills /
//! subagent) executing in the server's environment.
//!
//! # The web build IS the desktop frontend
//!
//! The user's requirement is explicit: the cloud web version should feel like "the
//! desktop frontend, running on a server" — the same React UI, the same design
//! language, the same `AgentEventWire` contract. The only difference is **where the
//! SDK data is processed**: on the server (this module) rather than in the Tauri
//! process. So instead of building a second UI, we serve the desktop React build
//! (`src/interface/desktop/web/dist`) verbatim and swap the transport in `bridge.ts`
//! from Tauri `invoke` / `listen` to HTTP (`POST /api/cmd/{name}`) + one SSE stream
//! (`GET /api/events`).
//!
//! # Thin transport over the shared core
//!
//! All orchestration — the project registry, the lazily-loaded per-project runtimes,
//! the turn driver, the wire DTOs — lives in [`super::runtime::ProjectRuntimeManager`],
//! shared verbatim with the Tauri shell (`interface/desktop/shell.rs`). This module is
//! only the HTTP/SSE skin: it maps `POST /api/cmd/{name}` onto core methods and pipes
//! the core's event sink into a global `broadcast` that `GET /api/events` fans out as
//! `text/event-stream`.
//!
//! # Transport contract
//!
//! - `POST /api/cmd/{name}` — one endpoint per `AgentBridge` method (body = args JSON,
//!   response = JSON or a plain-text error with a non-2xx status). See [`dispatch`].
//! - `GET /api/events` — Server-Sent Events. Each event is `event: agent_event` +
//!   `data: <AgentEventWire json>`, tagged with `project` + `session` so the frontend
//!   routes events to the right session view.
//! - everything else — the desktop React build, served from `dist` with an SPA fallback.

#![cfg(feature = "server")]

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};

use super::desktop::wire::AgentEventWire;
use super::runtime::{
    EventSink, FileEntryWire, HistoryMessageWire, ModelEntryWire, ProjectEntryWire,
    ProjectRuntimeManager, SessionEntryWire, SkillEntryWire, SubagentSummaryWire,
};
use crate::bootstrap::AgentFactory;
use crate::project::ProjectRegistry;

const EVENT_CHANNEL_CAPACITY: usize = 256;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct ServerState {
    manager: Arc<ProjectRuntimeManager>,
    /// Global fan-out of serialized wire events to every connected SSE client.
    events: broadcast::Sender<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Start the cloud web server (async: runs on the shared tokio runtime).
pub async fn run(factory: AgentFactory) -> std::io::Result<()> {
    let working_dir = factory.working_dir.clone();
    let registry = ProjectRegistry::load_default().map_err(|e| std::io::Error::other(e.to_string()))?;
    let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

    // The event sink the core calls for every wire event: serialise + fan out.
    let sink_tx = events.clone();
    let sink: EventSink = Arc::new(move |wire: AgentEventWire| {
        if let Ok(json) = serde_json::to_string(&wire) {
            let _ = sink_tx.send(json);
        }
    });

    // Server mode has no `__chat__` pseudo project: only real registered projects.
    let manager = Arc::new(ProjectRuntimeManager::new(
        Arc::new(tokio::sync::Mutex::new(registry)),
        None,
        sink,
    ));
    // Register the server's default workspace so the UI always has a project to show.
    manager
        .register_default(working_dir)
        .await
        .map_err(std::io::Error::other)?;

    let state = Arc::new(ServerState { manager, events });

    // Serve the desktop React build. `dist` is produced by
    // `cd src/interface/desktop/web && npm run build`; override with `SHIRLEY_WEB_DIST`.
    let dist = std::env::var_os("SHIRLEY_WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/interface/desktop/web/dist")
        });
    let index = dist.join("index.html");
    let static_files = ServeDir::new(&dist).fallback(ServeFile::new(index));

    let app = Router::new()
        .route("/api/cmd/{cmd}", post(dispatch))
        .route("/api/events", get(events_handler))
        .fallback_service(static_files)
        .with_state(state);

    let addr = std::env::var("SHIRLEY_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| std::io::Error::other(format!("failed to bind {addr}: {e}")))?;
    eprintln!("[server] cloud web listening on http://{addr} (dist: {})", dist.display());
    axum::serve(listener, app)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

struct ApiError(StatusCode, String);

impl ApiError {
    fn bad(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, message.into())
    }
    fn internal(message: impl Into<String>) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, message.into())
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, message.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        ApiError::bad(message)
    }
}

fn ok_json<T: Serialize>(value: T) -> Result<Response, ApiError> {
    Ok(Json(value).into_response())
}

fn no_content() -> Result<Response, ApiError> {
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// Command arguments (one tolerant struct: the bridge sends a JSON object per command)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct CmdArgs {
    project: Option<String>,
    session: Option<String>,
    name: Option<String>,
    path: Option<String>,
    title: Option<String>,
    model: Option<String>,
    query: Option<String>,
    text: Option<String>,
    references: Option<Vec<String>>,
    images: Option<Vec<String>>,
    id: Option<String>,
    archived: Option<bool>,
    #[serde(rename = "targetProject")]
    target_project: Option<String>,
}

impl CmdArgs {
    fn project(&self) -> Result<String, ApiError> {
        self.project
            .clone()
            .ok_or_else(|| ApiError::bad("missing project"))
    }
    fn session(&self) -> Result<String, ApiError> {
        self.session
            .clone()
            .ok_or_else(|| ApiError::bad("missing session"))
    }
}

// ---------------------------------------------------------------------------
// Command dispatch
// ---------------------------------------------------------------------------

/// Single command endpoint: `POST /api/cmd/{name}` with a JSON args body. `name` is one
/// of the `AgentBridge` methods (see `web/src/lib/bridge.ts`); the response mirrors the
/// Tauri command's return type. This keeps the frontend's transport a thin, uniform
/// `fetch` shim — no per-command routing table on the client.
async fn dispatch(
    State(state): State<Arc<ServerState>>,
    Path(cmd): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let args: CmdArgs = if body.is_empty() {
        CmdArgs::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad(format!("bad args: {e}")))?
    };
    let manager = &state.manager;
    match cmd.as_str() {
        // -- projects ----------------------------------------------------------------
        "agent_list_projects" => {
            ok_json(manager.list_projects(args.archived.unwrap_or(false)).await)
        }
        "agent_add_project" => {
            let path = args.path.ok_or_else(|| ApiError::bad("missing path"))?;
            let entry = manager.add_project(PathBuf::from(path), args.name).await?;
            ok_json(Some(entry))
        }
        "agent_rename_project" => {
            let project = args.project()?;
            let name = args.name.ok_or_else(|| ApiError::bad("missing name"))?;
            manager.rename_project(&project, &name).await?;
            no_content()
        }
        "agent_archive_project" => {
            let project = args.project()?;
            manager.archive_project(&project).await?;
            no_content()
        }
        "agent_restore_project" => {
            let project = args.project()?;
            manager.restore_project(&project).await?;
            no_content()
        }

        // -- turn --------------------------------------------------------------------
        "agent_send" => {
            let project = args.project()?;
            let text = args.text.ok_or_else(|| ApiError::bad("missing text"))?;
            manager
                .send(
                    &project,
                    args.session,
                    text,
                    args.references.unwrap_or_default(),
                    args.images.unwrap_or_default(),
                )
                .await?;
            Ok(StatusCode::ACCEPTED.into_response())
        }
        "agent_resend" => {
            let project = args.project()?;
            let text = args.text.ok_or_else(|| ApiError::bad("missing text"))?;
            manager
                .resend(
                    &project,
                    args.session,
                    text,
                    args.references.unwrap_or_default(),
                    args.images.unwrap_or_default(),
                )
                .await?;
            Ok(StatusCode::ACCEPTED.into_response())
        }
        "agent_cancel" => {
            let project = args.project()?;
            let session = args
                .session
                .or(args.name)
                .ok_or_else(|| ApiError::bad("missing session"))?;
            manager.cancel(&project, &session).await;
            no_content()
        }

        // -- subscription (events are broadcast globally on the SSE stream) -----------
        "agent_subscribe" => {
            let project = args.project()?;
            let session = args.session()?;
            // The global SSE stream carries every event; the frontend buffers until this
            // snapshot lands, so a snapshot under the session lock is enough. We drop the
            // per-session receiver — SSE does not need the desktop pump channel.
            let (_rx, snapshot) = manager.subscribe(&project, &session).await?;
            ok_json(snapshot)
        }
        "agent_unsubscribe" => no_content(),

        // -- model -------------------------------------------------------------------
        "agent_model_name" => {
            let project = args.project()?;
            ok_json(manager.model_name(&project).await?)
        }
        "agent_list_models" => {
            let project = args.project()?;
            ok_json(manager.list_models(&project).await?)
        }
        "agent_set_model" => {
            let project = args.project()?;
            let model = args.model.ok_or_else(|| ApiError::bad("missing model"))?;
            manager.set_model(&project, &model).await?;
            no_content()
        }

        // -- workspace files / skills -------------------------------------------------
        "agent_search_files" => {
            let project = args.project()?;
            let query = args.query.unwrap_or_default();
            ok_json(manager.search_files(&project, query).await?)
        }
        "agent_list_skills" => {
            let project = args.project()?;
            ok_json(manager.list_skills(&project).await?)
        }
        "agent_activate_skill" => {
            let project = args.project()?;
            let session = args.session()?;
            let name = args.name.ok_or_else(|| ApiError::bad("missing name"))?;
            manager.activate_skill(&project, &session, &name).await?;
            no_content()
        }

        // -- sessions -----------------------------------------------------------------
        "agent_list_sessions" => {
            let project = args.project()?;
            ok_json(manager.list_sessions(&project).await?)
        }
        "agent_current_session" => {
            let project = args.project()?;
            match manager.current_session(&project).await {
                Ok(name) => ok_json(Some(name)),
                Err(_) => ok_json(Option::<String>::None),
            }
        }
        "agent_new_session" => {
            let project = args.project()?;
            let name = manager.new_session(&project, args.title).await?;
            ok_json(SessionEntryWire {
                label: name.clone(),
                name,
                preview: String::new(),
                modified_ms: 0,
                turns: 0,
                running: false,
            })
        }
        "agent_switch_session" => {
            let project = args.project()?;
            let name = args.name.ok_or_else(|| ApiError::bad("missing name"))?;
            manager.switch_session(&project, &name).await?;
            no_content()
        }
        "agent_rename_session" => {
            let project = args.project()?;
            let name = args.name.ok_or_else(|| ApiError::bad("missing name"))?;
            let title = args.title.ok_or_else(|| ApiError::bad("missing title"))?;
            manager.rename_session(&project, &name, &title).await?;
            no_content()
        }
        "agent_delete_session" => {
            let project = args.project()?;
            let name = args.name.ok_or_else(|| ApiError::bad("missing name"))?;
            manager.delete_session(&project, &name).await?;
            no_content()
        }
        "agent_move_session" => {
            let project = args.project()?;
            let name = args.name.ok_or_else(|| ApiError::bad("missing name"))?;
            let target = args
                .target_project
                .ok_or_else(|| ApiError::bad("missing targetProject"))?;
            manager.move_session(&project, &name, &target).await?;
            no_content()
        }
        "agent_load_history" => {
            let project = args.project()?;
            ok_json(manager.load_history(&project).await?)
        }

        // -- subagents ----------------------------------------------------------------
        "agent_list_subagents" => {
            let project = args.project()?;
            let session = args.session()?;
            ok_json(manager.list_subagents(&project, &session).await?)
        }
        "agent_load_subagent" => {
            let project = args.project()?;
            let session = args.session()?;
            let id = args.id.ok_or_else(|| ApiError::bad("missing id"))?;
            ok_json(manager.load_subagent(&project, &session, &id).await?)
        }
        "agent_cancel_subagent" => {
            let project = args.project()?;
            let session = args.session()?;
            let id = args.id.ok_or_else(|| ApiError::bad("missing id"))?;
            manager.cancel_subagent(&project, &session, &id).await?;
            no_content()
        }

        other => Err(ApiError::not_found(format!("unknown command: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// SSE event stream
// ---------------------------------------------------------------------------

async fn events_handler(
    State(state): State<Arc<ServerState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.events.subscribe();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(json) => {
                    let event = Event::default().event("agent_event").data(json);
                    return Some((Ok::<_, Infallible>(event), rx));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
