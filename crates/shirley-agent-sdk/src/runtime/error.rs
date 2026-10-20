use crate::adapter;
use crate::tool;

/// The top-level error the SDK exposes.
///
/// It only "converges": each layer keeps its own error type and classification, and this
/// one collects them via `#[from]` instead of inventing its own `String` variants. The
/// upper layer only needs to ask [`AgentError::is_retryable`] and need not care which
/// layer the error came from.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Tool(#[from] tool::ToolError),

    #[error(transparent)]
    Adapter(#[from] adapter::AdapterError),

    #[error(transparent)]
    Sandbox(#[from] crate::sandbox::SandboxError),

    #[error(transparent)]
    Workspace(#[from] crate::workspace::WorkspaceError),

    #[error("[compression failed]: {0}")]
    Compression(String),

    /// Fallback variant: collects errors that don't fit the layers above (e.g. an
    /// application-level config / IO error).
    ///
    /// It deliberately holds a `Box<dyn Error>` instead of a `String`: this keeps the
    /// [`std::error::Error::source`] chain intact, so an external caller can
    /// `downcast_ref` to recover the original error type and handle it structurally,
    /// rather than guessing from the `to_string()` text.
    #[error("{0}")]
    Other(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl crate::error::SdkError for AgentError {
    fn kind(&self) -> crate::error::ErrorKind {
        use crate::error::ErrorKind;
        match self {
            Self::Tool(e) => e.kind(),
            Self::Adapter(e) => e.kind(),
            // Sandbox unavailable / constraint unsupported — changing the config is the
            // only thing that helps; retrying is pointless.
            Self::Sandbox(_) => ErrorKind::Unsupported,
            Self::Workspace(_) => ErrorKind::BadRequest,
            // A compression failure aborts the whole turn, but retrying the same context
            // usually won't help.
            Self::Compression(_) | Self::Other(_) => ErrorKind::Internal,
        }
    }
}
