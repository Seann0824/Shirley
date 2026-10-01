//! ReAct 运行时：Agent 主循环、事件、错误与上下文压缩。
//!
//! 按职责拆成几个子模块，`mod.rs` 只负责接线与再导出：
//!
//! - [`agent`]：`Agent` 本体（构造、主循环、压缩调度）。
//! - [`compaction`]：压缩切点计算与重建（`CutPlan` / `CompactParts` / `plan_cut`）。
//! - [`event`]：对外事件与运行结果（`AgentEvent` / `RunResult` / `StopReason`）。
//! - [`error`]：顶层错误收敛（`AgentError`）。

mod agent;
mod compaction;
mod error;
mod event;

pub use agent::Agent;
pub use compaction::{CompactParts, CutPlan, plan_cut};
pub use error::AgentError;
pub use event::AgentEvent;
