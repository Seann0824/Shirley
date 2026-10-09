use crate::adapter;
use crate::tool;

/// SDK 对外暴露的顶层错误。
///
/// 它只做"收敛"：每一层的错误保留自己的类型与分类，
/// 这里通过 `#[from]` 把它们收进来，不再自己发明 `String` 变体。
/// 上层只需要问 [`AgentError::is_retryable`]，不必关心错误来自哪一层。
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

    /// 兜底变体：收纳无法归入上面各层的错误（例如应用层的配置 / IO 错误）。
    ///
    /// 刻意持有 `Box<dyn Error>` 而不是 `String`：这样 [`std::error::Error::source`]
    /// 链条不断，外部调用方可以 `downcast_ref` 拿到原始错误类型并结构化处理，
    /// 而不是只能对着 `to_string()` 的文本猜。
    #[error("{0}")]
    Other(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl crate::error::SdkError for AgentError {
    fn kind(&self) -> crate::error::ErrorKind {
        use crate::error::ErrorKind;
        match self {
            Self::Tool(e) => e.kind(),
            Self::Adapter(e) => e.kind(),
            // 沙盒不可用 / 不支持该约束 —— 换配置才有意义，重试无用。
            Self::Sandbox(_) => ErrorKind::Unsupported,
            Self::Workspace(_) => ErrorKind::BadRequest,
            // 压缩失败会中断整轮任务，但重试同一份上下文通常不会变好。
            Self::Compression(_) => ErrorKind::Internal,
            Self::Other(_) => ErrorKind::Internal,
        }
    }
}
