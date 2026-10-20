//! Sandbox execution result.
//!
//! stderr / `exit_code` / degradation info are deliberately kept here:
//! the model needs to see failure details to self-correct (see docs/security.md 6.1),
//! and auditing needs to know "whether isolation was actually in effect at the time".

// Bring in serde's Serialize macro: lets this struct be serialized to JSON.
// Use: audit logs need to write it to a file, or send it somewhere over the network.
use serde::Serialize;

// Derive three capabilities:
// - Debug: can be printed with {:?}, handy for debugging
// - Clone: can be copied
// - Serialize: can be turned into JSON
#[derive(Debug, Clone, Serialize)]
pub struct SandboxOutput {
    // The subprocess's standard output (what it prints normally).
    pub stdout: String,
    // The subprocess's standard error (error messages).
    pub stderr: String,

    /// The process exit code. `None` when killed by a signal.
    // Option<i32>: either some integer Some(0)/Some(3), or None (e.g. force-killed with no exit code).
    pub exit_code: Option<i32>,

    /// Whether the process was force-terminated by the outer layer due to a timeout.
    pub timed_out: bool, // true / false

    /// The isolation-strength label actually in effect (from backend.capabilities().label).
    pub isolation: String,

    /// Descriptions of constraints that could not be satisfied at runtime and were degraded.
    /// For example `ProcessBackend` fills in "no filesystem isolation / no network isolation".
    /// The caller can use this to decide whether to reject the result (it should in high-risk
    /// scenarios).
    pub degraded: Vec<String>, // a list of descriptions of "things I failed to do"
}

// Add two convenience methods to SandboxOutput.
impl SandboxOutput {
    /// Whether "full isolation" is met — no degradation entries at all.
    // &self: borrow self to read it (does not take ownership, just a look).
    // -> bool: returns true/false.
    #[must_use]
    pub fn is_fully_isolated(&self) -> bool {
        // degraded is an empty list -> no degradation entries -> fully isolated.
        // .is_empty() returns true when the list is empty.
        self.degraded.is_empty()
    }

    /// Whether the command exited successfully.
    #[must_use]
    pub fn success(&self) -> bool {
        // Both conditions must hold to count as success:
        // 1. the exit code is exactly 0 (exit_code is Some(0))
        // 2. it did not time out
        // && is "and". == is "equals".
        self.exit_code == Some(0) && !self.timed_out // ! is "not"; !true is false
    }
}
