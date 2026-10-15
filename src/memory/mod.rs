//! Memory system (**an application-layer capability**, not an SDK primitive).
//!
//! See `docs/memory.md`. V1 scope:
//! - `core.md` always-on injection + `index.md` keyword-retrieval injection (through the SDK's
//!   `ContextProvider` seam, appending one system message at the end of each request — same as
//!   the task ledger);
//! - an incremental curator after a session ends (same-model extraction + deterministic
//!   self-check);
//! - temporal conflict fields (`supersedes` / `valid_from`; conflicts don't delete history).
//!
//! It lives in the app layer for the same reason as `todo.rs`: what to record, how to retrieve,
//! and what text to inject are trade-offs of "the coding-agent product". The SDK provides only
//! the generic `ContextProvider` seam; this module adds no new SDK public types.

mod curator;
mod embed;
mod format;
mod index;
mod provider;
mod store;
mod tool;
mod vector;

// Re-export only the types actually consumed **outside** the module (keep the facade small):
//   - `bootstrap.rs`: `curate` / `CurateOutcome` / `CuratorError` /
//     `MemoryContextProvider` / `MemoryRuntime` / `MemoryStore`
//   - `interface/session.rs`: `MemoryRuntime`
// Everything else (`Entry` / `EntryType` / `search` / `write_index` / `Candidate` ...) is an
// internal building block referenced across submodules via `super::`, so it doesn't need to be
// hoisted to `crate::memory::`.
pub use curator::{ConsolidateOutcome, CurateOutcome, CuratorError, consolidate, curate};
pub use embed::{Embedder, EmbeddingConfig};
pub use provider::{MemoryContextProvider, MemoryRuntime};
pub use store::MemoryStore;
pub use tool::RecallTool;
