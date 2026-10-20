//! Application-layer subagent capability (`docs/subagent.md`).
//!
//! A **subagent** is a same-process, application-layer child `Agent` dispatched by the parent
//! model through the ordinary `subagent` tool. There is **zero SDK change**: the dispatch entry is
//! a plain [`Tool`](shirley_agent_sdk::Tool), the child is a plain `Agent` built by
//! [`AgentFactory`](crate::bootstrap::AgentFactory), and the "parent sleeps / wakes" is just the
//! tool future staying `Pending` while the child runs (the runtime's `FuturesUnordered` awaits it
//! naturally, and concurrency comes for free).
//!
//! Two primitives (decision 3):
//! - **spawn**: a clean-context child (`build_agent(vec![])`); only the `task` is visible to it.
//! - **fork**: the child inherits a snapshot of the parent conversation (the parent's working set
//!   with the leading `System` stripped, so the child's own regenerated system is the only one).
//!
//! A fork child **never writes back** to the parent's `self.messages` (hard constraint): the only
//! return path is the tool result. `SubagentManager` keeps a **derived mirror** of the parent's
//! working set (`parent_view`) purely as the fork snapshot source, fed by `Session::apply_event`.
//!
//! Pieces:
//! - [`manager`]: the registry, the three caps, the recursion guard, `wait` / `cancel` and the
//!   per-subagent real-time channel (decision 10, path 2 -- the SDK's `Tool` cannot emit events).
//! - [`store`]: the `.agents/` sidecar (`index.jsonl` + `<id>.jsonl`), decision 9.
//! - [`tool`]: the single `subagent` tool the parent model calls.

mod manager;
mod store;
mod tool;

pub use manager::{
    DEFAULT_MAX_CONCURRENT, DEFAULT_MAX_DEPTH, DEFAULT_PER_TASK_MAX_STEPS, DEFAULT_WAIT_TIMEOUT_MS,
    SubagentError, SubagentEvent, SubagentInfo, SubagentKind, SubagentLimits, SubagentManager,
    SubagentStatus, ToolSelection, WaitOutcome,
};
pub use store::{IndexRecord, SubagentStore};
pub use tool::SubagentTool;
