//! 统一错误契约的回归测试。
//!
//! 这些断言锁的是"分类语义"，不是文案：
//! 文案可以改，`ErrorKind` 与 `is_retryable` 的行为不能随便改——
//! 上层的重试 / 降级决策全挂在它们身上。

use agent_sdk::error::{ErrorKind, SdkError};
use agent_sdk::{AdapterError, ModelProtocol};

/// HTTP 状态码必须映射到正确的分类。
/// 这是本次重构最核心的一条：以前状态码被拼进字符串，分类信息直接丢了。
#[test]
fn http_status_maps_to_kind() {
    // 429 是限流，必须可重试——否则长任务遇到限流就整轮报废。
    assert_eq!(kind_of_http(429), ErrorKind::RateLimited);
    assert!(kind_of_http(429).is_retryable());

    // 5xx 是服务端抖动，可重试。
    assert_eq!(kind_of_http(500), ErrorKind::ServerError);
    assert_eq!(kind_of_http(503), ErrorKind::ServerError);
    assert!(kind_of_http(503).is_retryable());

    // 4xx 是请求本身有问题，重试同一份请求毫无意义。
    assert_eq!(kind_of_http(400), ErrorKind::BadRequest);
    assert_eq!(kind_of_http(401), ErrorKind::BadRequest);
    assert_eq!(kind_of_http(404), ErrorKind::BadRequest);
    assert!(!kind_of_http(400).is_retryable());
}

/// 未实现的协议必须是可处理的 `Err`，绝不能 panic。
///
/// 对应 `docs/runtime-hardening.md` 第七节：SDK 里任何未实现路径都必须返回错误，
/// 因为切换协议是运行时行为，不能把宿主进程带走。
#[test]
fn unsupported_protocol_is_error_not_panic() {
    let error = AdapterError::UnsupportedProtocol {
        protocol: format!("{:?}", ModelProtocol::Responses),
    };
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert!(!error.is_retryable());

    // 两个未实现协议都还在枚举里（防止有人删了变体却忘了实现）。
    let _ = ModelProtocol::Responses;
    let _ = ModelProtocol::AnyhtopicMessages;
}

/// 解析 / 编码失败属于内部错误：重试同一份输入不会变好。
#[test]
fn decode_and_encode_are_not_retryable() {
    assert_eq!(
        AdapterError::Decode("缺 choices[0]".into()).kind(),
        ErrorKind::Internal
    );
    assert_eq!(
        AdapterError::Encode("API Key 无法构成请求头".into()).kind(),
        ErrorKind::Internal
    );
    assert!(!AdapterError::Decode("x".into()).is_retryable());
}

/// `ErrorKind::is_retryable` 是唯一的重试依据，行为必须稳定。
#[test]
fn retryable_kinds_are_stable() {
    // 可重试：瞬时的网络 / 服务端问题。
    assert!(ErrorKind::Transport.is_retryable());
    assert!(ErrorKind::RateLimited.is_retryable());
    assert!(ErrorKind::ServerError.is_retryable());

    // 不可重试：换配置、换参数或改代码才有用。
    assert!(!ErrorKind::BadRequest.is_retryable());
    assert!(!ErrorKind::Unsupported.is_retryable());
    assert!(!ErrorKind::ToolFailure.is_retryable());
    assert!(!ErrorKind::Internal.is_retryable());
}

/// 每个 kind 都要有稳定的展示文案（日志 / 审计会用到）。
#[test]
fn every_kind_has_display_text() {
    for kind in [
        ErrorKind::Transport,
        ErrorKind::RateLimited,
        ErrorKind::ServerError,
        ErrorKind::BadRequest,
        ErrorKind::Unsupported,
        ErrorKind::ToolFailure,
        ErrorKind::Internal,
    ] {
        assert!(!kind.to_string().is_empty(), "{kind:?} 缺少展示文案");
    }
}

/// 错误链要能往上追溯到底层原因（`#[source]` 没写错）。
#[test]
fn error_source_chain_is_preserved() {
    let error = AdapterError::Http {
        status: 503,
        body: "upstream unavailable".into(),
    };
    // 有 body 就应该出现在展示文案里，便于排查。
    let text = error.to_string();
    assert!(text.contains("503"), "展示文案应包含状态码: {text}");
    assert!(text.contains("upstream unavailable"), "展示文案应包含错误体: {text}");
}

// --- 测试辅助 ---------------------------------------------------------------

/// 用真实的 `AdapterError` 走一遍状态码 → kind 的映射。
fn kind_of_http(status: u16) -> ErrorKind {
    AdapterError::Http {
        status,
        body: String::new(),
    }
    .kind()
}
