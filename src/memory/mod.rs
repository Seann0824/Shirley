//! 记忆系统（**应用层能力**，不是 SDK 基础能力）。
//!
//! 设计见 `docs/memory.md`。V1 范围：
//! - `core.md` 常驻注入 + `index.md` 关键词检索注入（经 SDK 的 `ContextProvider`
//!   接缝，每轮请求末尾追加一条 system，与任务账本同款）；
//! - 会话结束后的增量 curator（同模型抽取 + 确定性自检）；
//! - 时间化冲突字段（`supersedes` / `valid_from`，冲突不删历史）。
//!
//! 归应用层的原因与 `todo.rs` 一致：记什么、怎么检索、注入什么文案是"coding
//! agent 这个产品"的取舍。SDK 只提供通用接缝 `ContextProvider`，本模块不新增
//! SDK 对外类型。

mod curator;
mod format;
mod index;
mod provider;
mod store;

// 只 re-export **模块外**真正消费的类型（保持门面小）：
//   - `bootstrap.rs`：`curate` / `CurateOutcome` / `CuratorError` /
//     `MemoryContextProvider` / `MemoryRuntime` / `MemoryStore`
//   - `interface/session.rs`：`MemoryRuntime`
// 其余（`Entry` / `EntryType` / `search` / `write_index` / `Candidate` …）是模块内部
// 构件，跨子模块用 `super::` 直接引用，不必抬到 `crate::memory::`。
pub use curator::{ConsolidateOutcome, CurateOutcome, CuratorError, consolidate, curate};
pub use provider::{MemoryContextProvider, MemoryRuntime};
pub use store::MemoryStore;
