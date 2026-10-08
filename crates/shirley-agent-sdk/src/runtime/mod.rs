//! ReAct 运行时：Agent 主循环、事件、错误与上下文压缩。
//!
//! 按职责拆成几个子模块，`mod.rs` 只负责接线与再导出：
//!
//! - [`agent`]：`Agent` 本体（构造、主循环、压缩调度）。
//! - [`compaction`]：压缩切点计算与重建（`CutPlan` / `CompactParts` / `plan_cut`）。
//! - [`event`]：对外事件与运行结果（`AgentEvent` / `RunResult` / `StopReason`）。
//! - [`error`]：顶层错误收敛（`AgentError`）。
//! - [`prompt`]：系统提示词（静态字符串或按运行时上下文动态生成）。

mod agent;
mod compaction;
mod error;
mod event;
mod prompt;

pub use agent::Agent;
pub use compaction::{CompactParts, CutPlan, plan_cut};
pub use error::AgentError;
pub use event::{AgentEvent, RunResult, StopReason};
pub use prompt::{SystemPrompt, SystemPromptContext};
