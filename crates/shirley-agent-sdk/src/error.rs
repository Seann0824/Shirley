//! The SDK's unified error contract.
//!
//! Design intent (consistent with section 2 of `docs/runtime-hardening.md`):
//!
//! 1. **Errors are defined in the module that owns them.** Only the "cross-layer shared
//!    contract" lives here — [`ErrorKind`] and [`SdkError`]; concrete error types
//!    (`AdapterError` / `ToolError` / `SandboxError` / `AgentError`) each stay in their own
//!    module, just like `SandboxError` does.
//! 2. **Decisions look at the kind, not the text.** When the upper layer must decide
//!    "retry or bubble up", it depends only on [`SdkError::is_retryable`], never matching
//!    on the error string.
//! 3. **Kinds must be stable.** [`ErrorKind`] is a stable enum for logging / metrics /
//!    audit; the text may change, the kind must not change casually.

use std::fmt;

/// A stable classification of an error. Used for logging, metrics, and retry decisions;
/// it does not change with the display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Network-layer failure: can't connect, connection reset, DNS failure, etc. Usually transient.
    Transport,
    /// Rate limited (HTTP 429). Retryable after backoff.
    RateLimited,
    /// Server error (HTTP 5xx). Usually retryable.
    ServerError,
    /// The request itself is bad (HTTP 4xx, except 429). Retrying is pointless.
    BadRequest,
    /// Capability unimplemented or backend unavailable. Retrying is pointless; change the
    /// config or the implementation.
    Unsupported,
    /// Tool execution failure. Retrying the same arguments usually won't help; the model
    /// needs a different strategy.
    ToolFailure,
    /// SDK-internal error (parsing, encoding, IO, etc.). Not retried by default.
    Internal,
}

impl ErrorKind {
    /// Whether this class of error is retryable by default.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Transport | Self::RateLimited | Self::ServerError
        )
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Transport => "transport failure",
            Self::RateLimited => "rate limited",
            Self::ServerError => "server error",
            Self::BadRequest => "bad request",
            Self::Unsupported => "unsupported",
            Self::ToolFailure => "tool failure",
            Self::Internal => "internal error",
        };
        f.write_str(text)
    }
}

/// The contract implemented by every error type in the SDK.
///
/// With it, the runtime (retry / degrade / cancel) and the presentation layer only face a
/// single interface, instead of matching each layer's enum one by one.
pub trait SdkError: std::error::Error + Send + Sync {
    /// Stable classification. Use this for logs and metrics, not `to_string()`.
    fn kind(&self) -> ErrorKind;

    /// Whether it can be retried. Defaults to deriving from [`ErrorKind::is_retryable`];
    /// a concrete error type may override it when the semantics are finer (e.g. "this 5xx
    /// is actually deterministic").
    fn is_retryable(&self) -> bool {
        self.kind().is_retryable()
    }
}
