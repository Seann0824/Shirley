//! Backend: turns a [`SandboxSpec`] into one real execution.
//!
//! This layer is abstracted out so the "isolation mechanism" is swappable:
//! - [`ProcessBackend`]: no isolation, for local development/testing only; never use it on
//!   untrusted input.
//! - macOS can wire in `sandbox-exec`; Linux can wire in `bwrap` / `nsjail` / `runsc`.
//!
//! The backend's responsibility boundary: **it only handles execution and isolation**, it makes no
//! permission decisions (that is the policy layer's job), and it does not handle timeouts (timeouts
//! are applied uniformly by [`super::Sandbox`], ensuring no backend can evade them).

// Bring in the types we need.
use crate::sandbox::output::SandboxOutput;
use crate::sandbox::spec::SandboxSpec; // the execution spec (what to run) // the execution result

// `pub mod process;` means "this directory has a submodule `process`",
// which it will find at backend/process.rs.
pub mod process;

// Re-export ProcessBackend from the process module,
// so callers can write backend::ProcessBackend instead of backend::process::ProcessBackend.
pub use process::ProcessBackend;

/// Self-describing backend capabilities.
///
/// The caller uses this to decide "whether the isolation I asked for is actually in effect".
/// (Note: this is the backend's "static declaration" of what it can do, not the actual result of
/// "this particular execution".)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Copy: this struct is small enough to copy directly (no move needed)
// PartialEq, Eq: comparable for equality (can use ==)
pub struct Capabilities {
    /// Whether filesystem isolation can be provided.
    pub filesystem_isolation: bool,
    /// Whether network can be cut off / egress forced through a proxy.
    pub network_isolation: bool,
    /// Whether memory / process count can be limited.
    pub resource_limits: bool,
    /// A human-readable label for the isolation strength, used in logs and auditing.
    // &'static str: a string constant that "lives until the program ends", e.g. "none (bare process)".
    pub label: &'static str,
}

/// A sandbox backend.
///
/// Note this is a synchronous trait describing capabilities; actual execution goes through
/// `execute` (async). Backend implementors must guarantee that even if a constraint in the
/// `SandboxSpec` cannot be enforced, it is reported faithfully in [`SandboxOutput`] rather than
/// silently ignored.
///
/// `trait` is an "interface/contract": whoever implements it must provide the methods below.
/// `: Send + Sync` means "this backend can be shared safely across threads".
pub trait SandboxBackend: Send + Sync {
    // Return this backend's capabilities (whether it can do filesystem isolation, network isolation,
    // etc.).
    // &self: reads self.
    fn capabilities(&self) -> Capabilities;

    /// Execute once. `Ok` means "the process finished and we captured the output"; it does not mean
    /// the command succeeded — command failure is expressed via [`SandboxOutput::exit_code`].
    /// `Err` only means "the sandbox itself could not execute" (e.g. the backend is unavailable).
    ///
    /// `impl Future<...>` is the return type of an "async function" (the underlying form of an
    /// `async fn` in Rust). `+ Send` means this Future can be sent across threads.
    fn execute(
        &self,
        spec: &SandboxSpec, // borrow the spec to read it; do not take ownership
    ) -> impl std::future::Future<Output = Result<SandboxOutput, SandboxError>> + Send;
    // Result<A, B>: either success Ok(A) or failure Err(B).

    /// Give this backend a short name, for audit logs.
    fn name(&self) -> &'static str;
}

/// The error type for when the sandbox itself fails. `enum` = choose one.
///
/// The display format is uniformly `[prefix]: detail`, implemented via a thiserror derive —
/// consistent with `AdapterError` / `ToolError` / `WorkspaceError`.
/// The prefix stays next to the variant: `BackendUnavailable` and `Unsupported` both belong to
/// `ErrorKind::Unsupported`, but the former is "this platform doesn't have it" while the latter is
/// "this constraint isn't accepted"; when troubleshooting you need to tell them apart.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The backend is unavailable on the current platform (e.g. trying to use sandbox-exec on
    /// Linux).
    #[error("[sandbox unavailable]: {0}")]
    BackendUnavailable(String),

    /// The backend failed to start.
    #[error("[sandbox spawn failed]: {0}")]
    Spawn(String),

    /// The backend does not support one of the constraints required by the spec, and degradation is
    /// not allowed.
    #[error("[unsupported sandbox constraint]: {0}")]
    Unsupported(String),

    /// Underlying IO error. `#[from]` provides the automatic `io::Error -> SandboxError`
    /// conversion, so call sites can keep using `?`; `#[source]` preserves the error chain for
    /// upward tracing.
    #[error("[sandbox io error]: {0}")]
    Io(
        #[from]
        #[source]
        std::io::Error,
    ),
}

// Hook into the SDK's unified error contract: lets callers use the same interface to judge
// retryability.
impl crate::error::SdkError for SandboxError {
    fn kind(&self) -> crate::error::ErrorKind {
        use crate::error::ErrorKind;
        match self {
            // The backend is unavailable on the current platform / does not support the constraint:
            // swap the implementation or the configuration; retrying is useless.
            Self::BackendUnavailable(_) | Self::Unsupported(_) => ErrorKind::Unsupported,
            // Spawn failures and IO errors may be transient (fork failure, resource pressure).
            Self::Spawn(_) | Self::Io(_) => ErrorKind::Internal,
        }
    }
}
