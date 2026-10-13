//! Process sandbox.
//!
//! Design intent (consistent with docs/security.md):
//!
//! 1. **The boundary is "what can be touched", not "what the command string looks like".**
//!    So the spec uses program + args, not `bash -c "..."`.
//! 2. **Deny by default.** Network is off by default, env vars are not inherited by default, and
//!    write paths are empty by default.
//! 3. **Swappable backend.** The isolation mechanism is provided by [`backend::SandboxBackend`]:
//!    local development uses [`backend::ProcessBackend`], production swaps in bwrap / sandbox-exec /
//!    microVM.
//! 4. **Timeouts cannot be evaded.** The timeout is applied at the outermost layer by [`Sandbox`]
//!    with tokio; even if the backend tries to hang on, it will be dropped + killed.
//! 5. **Degradation is transparent.** Constraints the backend cannot enforce must be written into
//!    [`SandboxOutput::degraded`], so the caller can reject the result instead of being fooled by
//!    "it looked like it ran".
//!
//! Typical usage:
//!
//! ```no_run
//! use shirley_agent_sdk::sandbox::{Sandbox, SandboxSpec, backend::ProcessBackend};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let sandbox = Sandbox::new(ProcessBackend::default());
//! let spec = SandboxSpec::new("python3")
//!     .arg("-c")
//!     .arg("print('hi')")
//!     .workspace_root(".");
//! let out = sandbox.run(&spec).await?;
//! println!("{}", out.stdout);
//! # Ok(()) }
//! ```

// Declare the three submodules, each corresponding to one file.
pub mod backend; // backend/mod.rs
pub mod output;  // output.rs
pub mod spec;    // spec.rs

// Re-export common types so callers can write one less path segment.
// E.g. sandbox::SandboxSpec instead of sandbox::spec::SandboxSpec.
pub use backend::{Capabilities, SandboxBackend, SandboxError};
pub use output::SandboxOutput;
pub use spec::{NetworkPolicy, SandboxSpec};

// Import Duration (the time-span type).
use std::time::Duration;

/// Sandbox facade: holds one backend and is responsible for applying the timeout at the outermost
/// layer and assembling the result.
///
/// `<B: SandboxBackend>` is a generic parameter: a Sandbox can hold any kind of backend. This way,
/// swapping backends requires zero changes to the Sandbox layer itself.
pub struct Sandbox<B: SandboxBackend> {
    // The backend held internally; the one that actually does the work.
    backend: B,
    /// Overrides the timeout in the spec; `None` means use the spec's own.
    hard_timeout: Option<Duration>,
}

// `impl<B: SandboxBackend> Sandbox<B>`: add methods to "a Sandbox over any backend".
impl<B: SandboxBackend> Sandbox<B> {
    // Constructor: pass in a backend, get a Sandbox.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            hard_timeout: None, // no hard timeout cap by default
        }
    }

    /// Set a hard timeout cap that the spec cannot relax.
    // mut self + returns self: chained style.
    pub fn with_hard_timeout(mut self, timeout: Duration) -> Self {
        self.hard_timeout = Some(timeout);
        self
    }

    // Pass the backend's capabilities through so callers can query them.
    pub fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }

    /// Execute once.
    ///
    /// The timeout is applied uniformly here: even if the backend has its own timeout internally,
    /// this is the last line of defense.
    pub async fn run(&self, spec: &SandboxSpec) -> Result<SandboxOutput, SandboxError> {
        // Decide how long a timeout to actually use this time.
        // match handles two cases depending on whether hard_timeout has a value.
        let effective = match self.hard_timeout {
            // Has a hard cap: take the smaller of "what the spec wants" and "the hard cap".
            // .min(...) returns the smaller of the two.
            Some(hard) => spec.timeout.min(hard),
            // No hard cap: use the spec's own.
            None => spec.timeout,
        };

        // Clone the spec and set its timeout to the computed effective value.
        // clone() copies it, so the original spec passed in by the caller is not modified.
        let mut effective_spec = spec.clone();
        effective_spec.timeout = effective;

        // The key step: wrap the backend execution with tokio's timeout.
        // tokio::time::timeout(duration, future) interrupts the future on timeout.
        match tokio::time::timeout(effective, self.backend.execute(&effective_spec)).await {
            // The backend finished before the timeout; return its result directly.
            Ok(result) => result,
            // Timed out (Err(_) carries the timeout info; we don't use it).
            Err(_) => {
                // Timeout: the backend future is dropped, and cleanup is done via kill_on_drop in
                // the backend. Here we hand-assemble a "timeout result" to return.
                Ok(SandboxOutput {
                    stdout: String::new(), // no output captured
                    // Build an explanatory message, e.g. "command timed out after 5s".
                    stderr: format!("command timed out after {}s", effective.as_secs()),
                    exit_code: None, // force-killed, no exit code
                    timed_out: true, // mark as timed out
                    // Record the label of the backend that was used.
                    isolation: self.backend.capabilities().label.to_owned(),
                    // A timeout is not an "isolation degradation", so the list is empty.
                    degraded: Vec::new(),
                })
            }
        }
    }
}
