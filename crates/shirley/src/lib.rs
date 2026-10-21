//! # Shirley — application crate
//!
//! A runnable coding-agent application (TUI + desktop + cloud server) built on
//! top of [`shirley_agent_sdk`]. The SDK owns business-agnostic agent
//! fundamentals (messages, tools, the ReAct runtime, sandbox, workspace); this
//! crate owns everything that is a *product* decision: how sessions are stored,
//! what the task ledger records, how memory is curated, how the interfaces are
//! assembled.
//!
//! ## Why this lib exists
//!
//! This crate used to be a **binary-only** target with every module hung off
//! `main.rs`, which made the module graph impossible to see (`cargo doc` produced
//! nothing) and left no stable facade. It is now a normal lib + a thin
//! `main.rs` shell. The public surface is deliberately **tiny** ([`run`]): this
//! is an application, not a library, so its internals stay `pub(crate)` and are
//! documented here rather than exported. Run `cargo doc --document-private-items`
//! to browse the full module tree.
//!
//! ## Layering (dependency direction points downward)
//!
//! ```text
//! interface/        ← rendering: tui / desktop / server
//!   ├─ session/     ← interface-agnostic multi-session orchestration (shared)
//!   └─ runtime/     ← desktop+server shared runtime state (shared)
//! bootstrap.rs      ← assembly: builds the AgentFactory everything hangs off
//!   prompt.rs       ← application-side system-prompt construction
//! domain/           ← application domain state & types (sessions, projects,
//!                      status projection, model catalog, task ledger, …)
//! infra/            ← infrastructure with no agent semantics (settings,
//!                      storage paths, remote cloud sync, prompt templating)
//! memory/ skills/ subagent/ tools/   ← capabilities
//! ```
//!
//! The `pub(crate) use` re-exports at the bottom keep the historical flat paths
//! (`crate::settings`, `crate::session`, …) working after the files moved into
//! `infra/` and `domain/`, so no call site had to change.
//!
//! ## Known structural debt (deliberately not fixed here)
//!
//! Four production **module cycles** remain; breaking them needs type
//! relocation, not directory moves: `settings ↔ storage`, `remote ↔ memory`,
//! `models ↔ model_providers`, `bootstrap ↔ subagent`. See `Agent.md` §8.

// ── entry / assembly layer ────────────────────────────────────────────────
pub(crate) mod bootstrap;
pub(crate) mod prompt;

// ── interfaces ────────────────────────────────────────────────────────────
pub(crate) mod interface;

// ── capabilities ──────────────────────────────────────────────────────────
pub(crate) mod memory;
pub(crate) mod skills;
pub(crate) mod subagent;
pub(crate) mod tools;

// ── domain & infrastructure (files live in subdirectories; flat paths are
//    re-exported below for backwards compatibility) ─────────────────────────
pub(crate) mod domain;
pub(crate) mod infra;

// ── flat-path compatibility re-exports (crate-internal) ───────────────────
pub(crate) use domain::{
    model_providers, models, permission, project, session, status, todo, workspace_search,
};
pub(crate) use infra::{prompt_template, remote, settings, storage};

use shirley_agent_sdk::AgentError;

/// Interface mode: TUI (default) or the desktop interface.
///
/// Selected by the `--desktop` launch flag; the `SHIRLEY_INTERFACE=desktop` env var works too.
/// Both interfaces share the same [`bootstrap::AgentFactory`] assembly (see `docs/desktop-interface.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Tui,
    Desktop,
    /// Cloud web interface: HTTP + WebSocket server, only-chat UI (no project
    /// management). Gated behind the `server` cargo feature (`--features server`).
    Server,
}

impl Mode {
    fn from_env_and_args() -> Self {
        let interface = std::env::var("SHIRLEY_INTERFACE").ok();
        if std::env::args().any(|arg| arg == "--server") || interface.as_deref() == Some("server") {
            Mode::Server
        } else if std::env::args().any(|arg| arg == "--desktop")
            || interface.as_deref() == Some("desktop")
        {
            Mode::Desktop
        } else {
            Mode::Tui
        }
    }
}

/// Application entry point: migrate legacy state, assemble the [`bootstrap::AgentFactory`],
/// start the shared sync worker, and hand off to the selected interface.
///
/// This is the crate's **only** public item. Everything else is `pub(crate)` — the
/// application is not a reusable library, so it exposes just this one door.
///
/// # Errors
///
/// Returns [`AgentError`] if legacy migration, assembly, or the interface run fails.
pub fn run() -> Result<(), AgentError> {
    let _ = dotenvy::dotenv();

    // All interfaces (desktop / TUI / cloud server) share one state root: `~/.shirley`.
    // Running them on the same machine under the same OS user intentionally means they
    // see the same projects / sessions / config. Move the legacy `<config_dir>/shirley`
    // root over once so the desktop's existing data does not appear lost.
    if storage::migrate_legacy_root().map_err(|error| AgentError::Other(Box::new(error)))? {
        eprintln!(
            "[storage] migrated legacy state root into {}",
            storage::global_root().display()
        );
    }

    // Workspace root: decides "which project this run is for", and is the source of workspace-level config.
    let working_dir = prompt::workspace_root();

    // Move legacy project-local state before any session files are opened.
    let migrated =
        storage::migrate_all(&working_dir).map_err(|error| AgentError::Other(Box::new(error)))?;
    if std::env::args().any(|arg| arg == "--migrate-data") {
        println!(
            "Migrated {migrated} legacy workspaces into {}",
            storage::global_root().display()
        );
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| AgentError::Other(Box::new(error)))?;

    let factory = runtime.block_on(async {
        // Assembly and catalog prewarming may spawn background tasks.
        let factory = bootstrap::AgentFactory::assemble(working_dir)?;
        factory.prewarm_model_catalog();
        // A local offline copy is used by every interface. The worker shares only personal entries.
        remote::start_sync_worker(storage::global_root());
        Ok::<_, AgentError>(factory)
    })?;

    match Mode::from_env_and_args() {
        Mode::Tui => runtime.block_on(interface::run(factory)),
        // Desktop interface: enter the Tauri event loop (blocks the main thread).
        Mode::Desktop => {
            // Share the live runtime, but enter Tauri outside any block_on.
            #[cfg(feature = "desktop")]
            tauri::async_runtime::set(runtime.handle().clone());
            interface::desktop::run(factory)
        }
        // Cloud web interface: run the HTTP + WebSocket server on the tokio runtime.
        #[cfg(feature = "server")]
        Mode::Server => runtime.block_on(interface::server::run(factory)),
        #[cfg(not(feature = "server"))]
        Mode::Server => Err(std::io::Error::other(
            "server mode requires building with `--features server`",
        )),
    }
    .map_err(|error| AgentError::Other(Box::new(error)))?;

    Ok(())
}
