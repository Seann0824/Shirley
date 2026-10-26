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

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};

use super::desktop::wire::{AgentEventWire, StateSnapshotWire};
use super::runtime::{EventSink, ProjectRuntimeManager, SessionEntryWire, StatusSink};
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
    /// Separate fan-out of **content-free** runtime-status snapshots (`/api/status`). Kept apart
    /// from `events` on purpose (see `docs/agent-status.md` §6.1): the status channel must tick
    /// even when no `AgentEvent` arrives.
    status_events: broadcast::Sender<String>,
    token: Option<String>,
    memory_root: PathBuf,
    sequence: Arc<AtomicU64>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Start the cloud web server (async: runs on the shared tokio runtime).
pub async fn run(factory: AgentFactory) -> std::io::Result<()> {
    let working_dir = factory.working_dir.clone();
    let registry =
        ProjectRegistry::load_default().map_err(|e| std::io::Error::other(e.to_string()))?;
    let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    let (status_events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

    // The event sink the core calls for every wire event: serialise + fan out.
    let sink_tx = events.clone();
    let sequence = Arc::new(AtomicU64::new(0));
    let sink_sequence = Arc::clone(&sequence);
    let sink: EventSink = Arc::new(move |wire: AgentEventWire| {
        if let Ok(mut json) = serde_json::to_value(&wire) {
            json["sequence"] = sink_sequence
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1)
                .into();
            let _ = sink_tx.send(json.to_string());
        }
    });

    // Content-free status sink: serialise the snapshot and fan out on its own channel.
    let status_tx = status_events.clone();
    let status_sink: StatusSink = Arc::new(move |wire: StateSnapshotWire| {
        if let Ok(json) = serde_json::to_string(&wire) {
            let _ = status_tx.send(json);
        }
    });

    // Chat uses the same persistent workspace as the desktop interface.
    let manager = Arc::new(
        ProjectRuntimeManager::new(
            Arc::new(tokio::sync::Mutex::new(registry)),
            Some(crate::storage::global_root().join("chat")),
            sink,
        )
        .with_status_sink(status_sink),
    );
    // Register the server's default workspace so the UI always has a project to show.
    manager
        .register_default(working_dir)
        .await
        .map_err(std::io::Error::other)?;

    let token = std::env::var("SHIRLEY_SERVER_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let state = Arc::new(ServerState {
        manager,
        events,
        status_events,
        token,
        sequence,
        memory_root: crate::storage::global_root().join("memory"),
    });

    // Serve the desktop React build. `dist` is produced by
    // `cd src/interface/desktop/web && npm run build`; override with `SHIRLEY_WEB_DIST`.
    let dist = std::env::var_os("SHIRLEY_WEB_DIST").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/interface/desktop/web/dist"),
        PathBuf::from,
    );
    let index = dist.join("index.html");
    let static_files = ServeDir::new(&dist).fallback(ServeFile::new(index));

    let app = Router::new()
        .route("/api/cmd/{cmd}", post(dispatch))
        .route("/api/events", get(events_handler))
        .route("/api/status", get(status_handler))
        .route("/api/memory/sync", post(memory_sync))
        .route("/api/memory/review", post(memory_review))
        .route(
            "/api/memory/confirm-personal",
            post(memory_confirm_personal),
        )
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorize,
        ))
        // Raw-byte upload (`docs/render-parts.md` inbound channels). It needs its own route
        // rather than an `/api/cmd` arm for two reasons: the shared JSON body limit is 16MiB
        // while a library entry may be 64MiB, and base64-in-JSON would inflate the body by a
        // third and parse it twice. Declared after `route_layer`, so it is authorized.
        .route(
            "/api/upload",
            post(upload_handler).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        // Deliberately *after* `route_layer`: an `<img src>` cannot carry an Authorization
        // header, so the unguessable handle is the capability. It is only mintable through the
        // authorized `agent_asset_url` command.
        .route("/api/asset/{handle}", get(asset_handler))
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
        .fallback_service(static_files)
        .with_state(Arc::clone(&state));

    let addr = std::env::var("SHIRLEY_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| std::io::Error::other(format!("failed to bind {addr}: {e}")))?;
    if !listener.local_addr()?.ip().is_loopback() && state.token.is_none() {
        return Err(std::io::Error::other(
            "SHIRLEY_SERVER_TOKEN is required when listening outside loopback",
        ));
    }
    eprintln!(
        "[server] cloud web listening on http://{addr} (dist: {})",
        dist.display()
    );
    axum::serve(listener, app)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))
}

async fn authorize(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.token.as_deref() else {
        return next.run(request).await;
    };
    let bearer = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    // Browser EventSource cannot attach headers. Native clients always use Bearer headers.
    let supplied = bearer.or_else(|| {
        (request.uri().path() == "/api/events")
            .then(|| {
                url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
                    .find(|(key, _)| key == "token")
                    .map(|(_, value)| value.into_owned())
            })
            .flatten()
    });
    let valid = supplied.as_deref().is_some_and(|value| {
        let mut difference = value.len() ^ expected.len();
        for (a, b) in value.bytes().zip(expected.bytes()) {
            difference |= (a ^ b) as usize;
        }
        difference == 0
    });
    if !valid {
        return (StatusCode::UNAUTHORIZED, "Cloud access token required").into_response();
    }
    next.run(request).await
}

async fn memory_sync(
    State(state): State<Arc<ServerState>>,
    Json(request): Json<crate::memory::sync::SyncRequest>,
) -> Result<Response, ApiError> {
    let root = state.memory_root.clone();
    let result = tokio::task::spawn_blocking(move || crate::memory::sync::exchange(&root, request))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(|e| ApiError::bad(e.to_string()))?;
    ok_json(result)
}

async fn memory_review(State(state): State<Arc<ServerState>>) -> Result<Response, ApiError> {
    let root = state.memory_root.clone();
    let entries = tokio::task::spawn_blocking(move || crate::memory::sync::review_legacy(&root))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(|e| ApiError::bad(e.to_string()))?;
    ok_json(entries)
}

#[derive(Deserialize)]
struct ConfirmMemories {
    approvals: Vec<crate::memory::sync::MemoryApproval>,
}

async fn memory_confirm_personal(
    State(state): State<Arc<ServerState>>,
    Json(args): Json<ConfirmMemories>,
) -> Result<Response, ApiError> {
    let root = state.memory_root.clone();
    tokio::task::spawn_blocking(move || {
        crate::memory::sync::confirm_personal(&root, &args.approvals)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::bad(e.to_string()))?;
    no_content()
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

#[expect(
    clippy::unnecessary_wraps,
    reason = "handler convention: every route arm returns Result<Response, ApiError> so `?` propagates"
)]
fn ok_json<T: Serialize>(value: T) -> Result<Response, ApiError> {
    Ok(Json(value).into_response())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "handler convention: every route arm returns Result<Response, ApiError> so `?` propagates"
)]
fn no_content() -> Result<Response, ApiError> {
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// Command arguments (one tolerant struct: the bridge sends a JSON object per command)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct CmdArgs {
    action: Option<String>,
    input: Option<serde_json::Value>,
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
    /// Local paths for `agent_ingest_paths` (desktop-parity inbound channel).
    paths: Option<Vec<String>>,
    /// Content type for `agent_ingest_bytes`.
    mime: Option<String>,
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
        "agent_model_providers" => {
            let action = args.action.ok_or_else(|| ApiError::bad("missing action"))?;
            let global = state
                .memory_root
                .parent()
                .ok_or_else(|| ApiError::internal("missing application root"))?;
            let result =
                crate::model_providers::command(global, &action, args.input.unwrap_or_default())
                    .await?;
            ok_json(result)
        }
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
            let (_rx, snapshot, cursor) = manager
                .subscribe_with_cursor(&project, &session, || state.sequence.load(Ordering::SeqCst))
                .await?;
            let mut value =
                serde_json::to_value(snapshot).map_err(|e| ApiError::internal(e.to_string()))?;
            value["sequence"] = cursor.into();
            ok_json(value)
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
            manager
                .set_model(&project, &model, args.session.as_deref())
                .await?;
            no_content()
        }

        // -- workspace files / skills -------------------------------------------------
        "agent_search_files" => {
            let project = args.project()?;
            let query = args.query.unwrap_or_default();
            ok_json(manager.search_files(&project, query).await?)
        }
        // Asset channel (`docs/render-parts.md` P1): an artifact id in, an opaque URL out.
        "agent_asset_url" => {
            let project = args.project()?;
            let id = args
                .id
                .clone()
                .ok_or_else(|| ApiError::bad("missing id".to_owned()))?;
            let handle = manager.asset_handle(&project, &id).await?;
            ok_json(format!("/api/asset/{handle}"))
        }
        "agent_asset_text" => {
            let project = args.project()?;
            let id = args
                .id
                .clone()
                .ok_or_else(|| ApiError::bad("missing id".to_owned()))?;
            // Served through the authorized command channel, not the public asset route: this
            // returns *source code* that the page then runs in a sandboxed frame.
            ok_json(manager.asset_text(&project, &id).await?)
        }
        // Inbound media (`docs/render-parts.md` inbound channels). Two doors into the same
        // library: local paths (the server usually runs on the user's machine too) and raw
        // bytes (the browser cannot hand us a path). Both reuse the jail, allow-list and size
        // cap of the artifact store -- there is no second storage layer for user uploads.
        "agent_ingest_paths" => {
            let project = args.project()?;
            let paths = args.paths.unwrap_or_default();
            let (entries, errors) = manager.ingest_paths(&project, paths).await?;
            ok_json(serde_json::json!({ "entries": entries, "errors": errors }))
        }
        "agent_ingest_bytes" => {
            let project = args.project()?;
            let mime = args
                .mime
                .ok_or_else(|| ApiError::bad("missing mime".to_owned()))?;
            let name = args.name.unwrap_or_else(|| "upload".to_owned());
            let data_url = args
                .text
                .ok_or_else(|| ApiError::bad("missing data".to_owned()))?;
            let (_, bytes) = crate::artifact::decode_data_url(&data_url)
                .map_err(|e| ApiError::bad(e.to_string()))?;
            ok_json(manager.ingest_bytes(&project, &name, &mime, &bytes).await?)
        }
        // The library is an inventory, not a conversation: it outlives any one session, so the
        // page can answer "which file did I send you last week?" without scrolling a transcript.
        "agent_library_list" => {
            let project = args.project()?;
            ok_json(manager.library_list(&project).await?)
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
        // No dictation route on purpose: transcription happens on the user's own device
        // (browser recogniser, or this computer's whisper.cpp in the desktop app). A phone
        // that opens a cloud project dictates with the phone's own ASR, so no audio ever
        // reaches this server. See `docs/voice-input.md`.
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

/// Serve one artifact (`docs/render-parts.md` P1). The jail already resolved the id when the
/// handle was minted, so this only reads a file it allowed; a forged handle is a plain 404.
///
/// Served as a **stream with Range support**, not one `read` into memory: a 64MiB clip would
/// otherwise be fully buffered per request, and `<video>` cannot seek at all without `206`
/// responses (browsers open media with a byte-range request and give up on a plain `200`).
async fn asset_handler(
    State(state): State<Arc<ServerState>>,
    Path(handle): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some((path, mime)) = state.manager.asset_by_handle(&handle) else {
        return (StatusCode::NOT_FOUND, "unknown asset handle").into_response();
    };
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return (StatusCode::NOT_FOUND, "unreadable asset").into_response();
    };
    let Ok(meta) = file.metadata().await else {
        return (StatusCode::NOT_FOUND, "unreadable asset").into_response();
    };
    let total = meta.len();
    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_range(value, total));
    // `no-store` kept: content is user data, and an opaque handle is not a cache key we want
    // shared between users of the same origin.
    //
    // An HTML artifact is never *navigated* to: the chat renders it through `asset_text` into a
    // `srcdoc` iframe without `allow-same-origin`. Serving it here as `text/html` would instead
    // hand the browser a same-origin script host keyed by an opaque handle -- the exact injection
    // surface this channel refuses to open -- so it goes out as inert text.
    let content_type = if crate::artifact::is_interactive_html(mime) {
        "text/plain; charset=utf-8"
    } else {
        mime
    };
    let common = [
        ("content-type", content_type),
        ("cache-control", "no-store"),
        ("accept-ranges", "bytes"),
        ("x-content-type-options", "nosniff"),
    ];
    match range {
        Some((start, end)) => {
            let len = end - start + 1;
            let body = read_range(file, start, len);
            (
                StatusCode::PARTIAL_CONTENT,
                common,
                [
                    ("content-range", format!("bytes {start}-{end}/{total}")),
                    ("content-length", len.to_string()),
                ],
                body,
            )
                .into_response()
        }
        None if headers.get(axum::http::header::RANGE).is_some() => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            common,
            [("content-range", format!("bytes */{total}"))],
        )
            .into_response(),
        None => (
            StatusCode::OK,
            common,
            [("content-length", total.to_string())],
            read_range(file, 0, total),
        )
            .into_response(),
    }
}

/// Parse a single-range `Range: bytes=a-b` header into an inclusive `(start, end)`.
///
/// Only the forms a media element actually sends are honoured (`a-b`, `a-`, `-n`); multi-range
/// requests fall back to the whole file, which is legal -- serving `200` with the full body is
/// always a valid answer to a range request. Returns `None` when the header asks for something
/// outside the file so the caller can answer `416` instead of silently sending everything.
fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    if spec.contains(',') || total == 0 {
        return None;
    }
    let (start, end) = match (start.trim(), end.trim()) {
        ("", suffix) => {
            let n: u64 = suffix.parse().ok()?;
            if n == 0 {
                return None;
            }
            (total.saturating_sub(n), total - 1)
        }
        (start, "") => (start.parse::<u64>().ok()?, total - 1),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(total - 1)),
    };
    if start > end || start >= total {
        return None;
    }
    Some((start, end))
}

/// Stream `len` bytes starting at `offset` in 64KiB chunks, so peak memory stays flat no matter
/// how large the artifact is.
fn read_range(file: tokio::fs::File, offset: u64, len: u64) -> axum::body::Body {
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
    let stream = futures::stream::try_unfold(
        (file, offset, len),
        |(mut file, remaining_offset, remaining)| async move {
            if remaining == 0 {
                return Ok(None);
            }
            file.seek(std::io::SeekFrom::Start(remaining_offset))
                .await?;
            let want = remaining.min(64 * 1024);
            let mut buf = vec![0_u8; want as usize];
            let read = file.read(&mut buf).await?;
            if read == 0 {
                return Ok(None);
            }
            buf.truncate(read);
            Ok::<_, std::io::Error>(Some((
                Bytes::from(buf),
                (
                    file,
                    remaining_offset + read as u64,
                    remaining - read as u64,
                ),
            )))
        },
    );
    axum::body::Body::from_stream(stream)
}

/// Largest inbound upload the library accepts; mirrors the jail's own cap so a too-large body is
/// rejected by the HTTP layer before any of it is buffered.
const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;

/// `POST /api/upload?project=<id>&name=<file>&mime=<type>` with the file as the **raw body**.
///
/// This is the cloud twin of the desktop's `agent_ingest_paths`: a browser cannot hand us a
/// path, so it hands us the bytes. The declared `mime` is advisory -- the jail re-derives and
/// validates it against its own allow-list, and a body that name does not match is refused
/// there, not trusted here.
async fn upload_handler(
    State(state): State<Arc<ServerState>>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let project = query
        .get("project")
        .ok_or_else(|| ApiError::bad("missing project".to_owned()))?;
    let mime = query
        .get("mime")
        .ok_or_else(|| ApiError::bad("missing mime".to_owned()))?;
    let name = query.get("name").map_or("upload", String::as_str);
    if body.is_empty() {
        return Err(ApiError::bad("empty upload".to_owned()));
    }
    let entry = state
        .manager
        .ingest_bytes(project, name, mime, &body)
        .await?;
    ok_json(entry)
}

async fn events_handler(
    State(state): State<Arc<ServerState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.events.subscribe();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        // Every branch returns, so a `loop` here would never actually loop (clippy::never_loop).
        match rx.recv().await {
            Ok(json) => {
                let event = Event::default().event("agent_event").data(json);
                Some((Ok::<_, Infallible>(event), rx))
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                Some((Ok(Event::default().event("resync").data("{}")), rx))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Content-free runtime-status SSE stream (`/api/status`). Same shape as `events_handler`
/// but on the separate `status_events` channel and with a distinct SSE event name, so a
/// client can subscribe to liveness/health without receiving any conversation content.
async fn status_handler(
    State(state): State<Arc<ServerState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.status_events.subscribe();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        // A `Lagged` status receiver simply skips the missed snapshots: a status snapshot is a
        // rebuildable projection, so there is nothing to "resync" — the next tick re-sends it.
        loop {
            match rx.recv().await {
                Ok(json) => {
                    let event = Event::default().event("status").data(json);
                    return Some((Ok::<_, Infallible>(event), rx));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bearer_auth_covers_commands_memory_and_sse_query_tokens_only_on_events() {
        let dir = std::env::temp_dir().join(format!(
            "shirley-cloud-auth-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let manager = Arc::new(ProjectRuntimeManager::new(
            Arc::new(tokio::sync::Mutex::new(
                ProjectRegistry::load(dir.join("projects.json")).unwrap(),
            )),
            None,
            Arc::new(|_| {}),
        ));
        let (events, _) = broadcast::channel(8);
        let (status_events, _) = broadcast::channel(8);
        let state = Arc::new(ServerState {
            manager,
            events,
            status_events,
            token: Some("secret".into()),
            memory_root: dir.join("memory"),
            sequence: Arc::new(AtomicU64::new(0)),
        });
        let app = Router::new()
            .route("/api/cmd/{cmd}", post(dispatch))
            .route("/api/memory/sync", post(memory_sync))
            .route("/api/memory/review", post(memory_review))
            .route(
                "/api/memory/confirm-personal",
                post(memory_confirm_personal),
            )
            .route("/api/events", get(events_handler))
            .route("/api/status", get(status_handler))
            .route_layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                authorize,
            ))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        for path in [
            "api/cmd/agent_list_projects",
            "api/cmd/agent_model_providers",
            "api/memory/sync",
            "api/memory/review",
        ] {
            let endpoint = format!("{base}/{path}");
            let body = if path.ends_with("agent_model_providers") {
                serde_json::json!({"action":"get"})
            } else {
                serde_json::json!({})
            };
            assert_eq!(
                client
                    .post(&endpoint)
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                client
                    .post(format!("{endpoint}?token=secret"))
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                client
                    .post(endpoint)
                    .bearer_auth("secret")
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
        let endpoint = format!("{base}/api/memory/confirm-personal");
        let body = serde_json::json!({"approvals": []});
        assert_eq!(
            client
                .post(&endpoint)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .post(&endpoint)
                .bearer_auth("secret")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            client
                .get(format!("{base}/api/events"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(format!("{base}/api/events?token=secret"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        task.abort();
        let _ = std::fs::remove_dir_all(dir);
    }
}
