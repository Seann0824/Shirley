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
// Computer use: drives other apps' windows through the macOS accessibility API. Application-layer
// on purpose -- "how to poke another app's UI" is a product decision, not an SDK fundamental.
pub(crate) mod computer;
pub(crate) mod memory;
// The task pool belongs to the personal-agent main line, not to the desktop ornament: the queue
// has to keep running on the cloud host while nobody is looking at a window, so every interface
// compiles it (`server` included -- `interface/server` drives `pool_tick_all` on a resident timer).
pub(crate) mod pool;
pub(crate) mod skills;
pub(crate) mod subagent;
pub(crate) mod tools;

// ── domain & infrastructure (files live in subdirectories; flat paths are
//    re-exported below for backwards compatibility) ─────────────────────────
pub(crate) mod domain;
pub(crate) mod infra;

// ── flat-path compatibility re-exports (crate-internal) ───────────────────
pub(crate) use domain::{bot, model_providers, models, session, status, todo};
pub(crate) use infra::{artifact, prompt_template, remote, settings, storage};
// Dictation is reached only from the desktop commands (`crate::asr::…`), so the flat alias is
// gated with them instead of warning as an unused import in a server-only build.
#[cfg(feature = "desktop")]
pub(crate) use infra::asr;

use shirley_agent_sdk::AgentError;

/// Interface mode: bot-first HTTP/SSE server.
///
/// The only supported runtime mode. The `SHIRLEY_INTERFACE` env var is kept for
/// forward compatibility but currently has no effect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Bot HTTP/SSE server (default and only mode).
    Server,
}

impl Mode {
    fn from_env_and_args() -> Self {
        Mode::Server
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

    // Headless one-shot entry (`shirley exec`): one prompt in, one answer out, then exit. It is
    // intercepted here, **before** state migration / assembly / the sync worker, because a caller
    // that fires one of these every few minutes must not pay — or mutate anything through — the
    // interactive startup path. See `interface::exec`.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).is_some_and(|arg| arg == "exec") {
        return interface::exec::run(&argv[2..]);
    }

    // All interfaces share one state root: `~/.shirley`.
    // Move the legacy `<config_dir>/shirley` root over once so the desktop's existing data does not appear lost.
    if storage::migrate_legacy_root().map_err(|error| AgentError::Other(Box::new(error)))? {
        eprintln!(
            "[storage] migrated legacy state root into {}",
            storage::global_root().display()
        );
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| AgentError::Other(Box::new(error)))?;

    let factory = runtime.block_on(async {
        // Assembly and catalog prewarming may spawn background tasks.
        let factory = bootstrap::AgentFactory::assemble()?;
        factory.prewarm_model_catalog();
        // A local offline copy is used by every interface. The worker shares only personal entries.
        remote::start_sync_worker(storage::global_root());
        Ok::<_, AgentError>(factory)
    })?;

    match Mode::from_env_and_args() {
        #[cfg(feature = "server")]
        Mode::Server => runtime
            .block_on(interface::server::run(factory))
            .map_err(|error| AgentError::Other(Box::new(error)))?,
        #[cfg(not(feature = "server"))]
        Mode::Server => {
            return Err(AgentError::Other(Box::new(std::io::Error::other(
                "server mode requires building with `--features server`",
            ))))
        }
    }

    Ok(())
}
