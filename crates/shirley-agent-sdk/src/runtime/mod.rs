//! ReAct runtime: the Agent main loop, events, errors, and context compaction.
//!
//! Split into submodules by responsibility; `mod.rs` only wires them up and re-exports:
//!
//! - [`agent`]: the `Agent` itself (construction, main loop, compaction scheduling).
//! - [`compaction`]: compaction cut-point computation and rebuild (`CutPlan` / `CompactParts` / `plan_cut`).
//! - [`event`]: public events and run results (`AgentEvent` / `RunResult` / `StopReason`).
//! - [`error`]: top-level error convergence (`AgentError`).
//! - [`prompt`]: the system prompt (a fixed string, or one generated from runtime context).

mod agent;
mod compaction;
mod context;
mod error;
mod event;
mod prompt;

pub use agent::Agent;
pub use compaction::{CompactParts, CompressionConfig, CutPlan, plan_cut};
pub use context::ContextProvider;
pub use error::AgentError;
pub use event::{AgentEvent, RunResult, StopReason};
pub use prompt::{SystemPrompt, SystemPromptContext};
