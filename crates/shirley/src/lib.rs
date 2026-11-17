//! # Shirley — bot-first agent platform
//!
//! An HTTP/SSE server that runs bots on the server side, serving a React web UI
//! to any browser. Built on top of [`shirley_agent_sdk`]. The SDK owns
//! business-agnostic agent fundamentals (messages, tools, the `ReAct` runtime,
//! sandbox); this crate owns everything that is a *product* decision: how
//! sessions are stored, what the task ledger records, how memory is curated,
//! how bots and their tool sets are assembled.
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
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "the only backend is macOS; other platforms compile the stub"
    )
)]
pub(crate) mod computer;
pub(crate) mod connectors;
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
pub(crate) use infra::{artifact, asr, prompt_template, remote, settings, storage};
use shirley_agent_sdk::AgentError;

/// Application entry point.
///
/// This is the crate's **only** public item. Everything else is `pub(crate)`.
///
/// # Errors
///
/// Returns [`AgentError`] if legacy migration, assembly, or the server fails.
pub fn run() -> Result<(), AgentError> {
    let _ = dotenvy::dotenv();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| AgentError::Other(Box::new(error)))?;

    runtime.block_on(async {
        let factory = bootstrap::AgentFactory::assemble()?;
        factory.prewarm_model_catalog();
        remote::start_sync_worker(storage::global_root());
        interface::server::run(factory)
            .await
            .map_err(|error| AgentError::Other(Box::new(error)))
    })
}
