//! SDK 统一错误契约。
//!
//! 设计意图（与 `docs/runtime-hardening.md` 第二节一致）：
//!
//! 1. **错误定义在拥有它的模块里。** 这里只放"跨层共享的契约"——
//!    [`ErrorKind`] 与 [`SdkError`]；具体错误类型（`AdapterError` / `ToolError` /
//!    `SandboxError` / `AgentError`）各自留在自己的模块，和 `SandboxError` 的做法一致。
//! 2. **决策看分类，不看文案。** 上层要做"重试还是冒泡"的判断时，
//!    只依赖 [`SdkError::is_retryable`]，绝不 `match` 错误字符串。
//! 3. **分类要稳定。** [`ErrorKind`] 是给日志 / 指标 / 审计用的稳定枚举，
//!    文案可以改，kind 不能随便改。

use std::fmt;

/// 错误的稳定分类。用于日志、指标与重试决策，不随展示文案变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 网络层失败：连不上、连接被重置、DNS 失败等。通常是瞬时的。
    Transport,
    /// 被限流（HTTP 429）。可退避后重试。
    RateLimited,
    /// 服务端错误（HTTP 5xx）。通常可重试。
    ServerError,
    /// 请求本身有问题（HTTP 4xx，429 除外）。重试没有意义。
    BadRequest,
    /// 能力未实现或后端不可用。重试没有意义，需要换配置或换实现。
    Unsupported,
    /// 工具执行失败。重试同一份参数通常不会变好，需要模型换策略。
    ToolFailure,
    /// SDK 内部错误（解析、编码、IO 等）。默认不重试。
    Internal,
}

impl ErrorKind {
    /// 这一类错误默认能不能重试。
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

/// SDK 内所有错误类型共同实现的契约。
///
/// 有了它，运行时（重试 / 降级 / 取消）与展示层都只需要面对一个接口，
/// 不必逐个 `match` 每一层的 enum。
pub trait SdkError: std::error::Error + Send + Sync {
    /// 稳定分类。日志与指标用这个，不要用 `to_string()`。
    fn kind(&self) -> ErrorKind;

    /// 能不能重试。默认由 [`ErrorKind::is_retryable`] 推导，
    /// 具体错误类型可以在语义更细时覆盖它（例如"这个 5xx 其实是确定性的"）。
    fn is_retryable(&self) -> bool {
        self.kind().is_retryable()
    }
}
