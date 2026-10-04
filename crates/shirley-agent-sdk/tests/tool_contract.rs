//! 工具层错误契约的回归测试。
//!
//! 锁的是这条承诺：**工具作者只需要返回 `ToolError`，不需要感知 `AgentError`。**
//!
//! 改造前 `ToolFuture` 的错误类型是 `AgentError`，宏里还写着
//! `map_err(::shirley_agent_sdk::AgentError::Tool)?`，等于强迫每个工具作者引用 SDK 的顶层错误类型。
//! 现在工具层只暴露 `ToolError`，由 runtime 在调用点收敛。

use shirley_agent_sdk::{ToolCall, ToolContext, ToolError, ToolManager, tool};

/// 构造一次工具调用。
///
/// `ToolCall` 是独立 struct，不是 `Message` 的变体——它只描述"模型要求调用哪个工具、
/// 带什么参数"，与消息流解耦，所以这里直接构造，而不是挂在 `Message` 上。
fn tool_call(id: &str, name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

/// 一个最普通的工具：成功路径。
/// 注意签名里只有 `ToolError`，没有 `AgentError`。
#[tool(description = "把两个数相加")]
async fn add(
    #[param(description = "左操作数")] left: i64,
    #[param(description = "右操作数")] right: i64,
) -> Result<i64, ToolError> {
    Ok(left + right)
}

/// 一个会失败的工具：用来验证错误能原样穿过宏与 ToolManager。
#[tool(description = "总是失败")]
async fn always_fails(
    #[param(description = "失败信息")] message: String,
) -> Result<String, ToolError> {
    Err(ToolError::ExecutionError(message))
}

/// 工具注册、调用、参数反序列化的完整链路。
#[tokio::test]
async fn tool_round_trip() {
    let mut manager = ToolManager::new();
    manager.register(add::tool()).expect("注册应成功");

    let call = tool_call("call_1", "add", r#"{"left": 2, "right": 3}"#);
    let output = manager.invoke(&call, ToolContext::new()).await.expect("调用应成功");

    assert_eq!(output, serde_json::json!(5));
}

/// 工具返回的错误必须能穿过宏，且分类正确。
#[tokio::test]
async fn tool_error_passes_through() {
    use shirley_agent_sdk::SdkError;

    let mut manager = ToolManager::new();
    manager.register(always_fails::tool()).expect("注册应成功");

    let call = tool_call("call_1", "always_fails", r#"{"message": "炸了"}"#);
    let error = manager.invoke(&call, ToolContext::new()).await.expect_err("应该失败");

    // 工具自己抛的 ExecutionError 必须原样保留，而不是被包成别的东西。
    assert!(
        matches!(error, ToolError::ExecutionError(ref m) if m == "炸了"),
        "错误应原样穿过宏，实际: {error:?}"
    );
    // 分类也必须对：工具执行失败，重试同一份参数没有意义。
    assert_eq!(error.kind(), shirley_agent_sdk::ErrorKind::ToolFailure);
    assert!(!error.is_retryable());
}

/// 参数不合法时应该是 `ArgumentsError`，而不是执行失败。
#[tokio::test]
async fn bad_arguments_are_reported_as_arguments_error() {
    let mut manager = ToolManager::new();
    manager.register(add::tool()).expect("注册应成功");

    // 缺字段
    let call = tool_call("call_1", "add", r#"{"left": 2}"#);
    let error = manager.invoke(&call, ToolContext::new()).await.expect_err("缺字段应失败");
    assert!(
        matches!(error, ToolError::ArgumentsError(_)),
        "缺字段应是 ArgumentsError，实际: {error:?}"
    );

    // arguments 根本不是 JSON
    let call = tool_call("call_2", "add", "not json");
    let error = manager.invoke(&call, ToolContext::new()).await.expect_err("非法 JSON 应失败");
    assert!(
        matches!(error, ToolError::ArgumentsError(_)),
        "非法 JSON 应是 ArgumentsError，实际: {error:?}"
    );
}

/// 调用不存在的工具应该是 `NotFoundError`。
#[tokio::test]
async fn unknown_tool_is_not_found() {
    let manager = ToolManager::new();
    let call = tool_call("call_1", "nope", "{}");
    let error = manager.invoke(&call, ToolContext::new()).await.expect_err("不存在的工具应失败");
    assert!(
        matches!(error, ToolError::NotFoundError(_)),
        "应是 NotFoundError，实际: {error:?}"
    );
}

/// 重复注册应该是 `RepetitionError`。
#[test]
fn duplicate_registration_is_rejected() {
    let mut manager = ToolManager::new();
    manager.register(add::tool()).expect("首次注册应成功");
    let error = manager.register(add::tool()).expect_err("重复注册应失败");
    assert!(
        matches!(error, ToolError::RepetitionError(_)),
        "应是 RepetitionError，实际: {error:?}"
    );
}

/// 工具层错误要能自动收敛成顶层错误（`#[from]` 生效）。
#[test]
fn tool_error_converges_into_agent_error() {
    use shirley_agent_sdk::AgentError;

    let agent_error: AgentError = ToolError::NotFoundError("nope".into()).into();
    // `transparent` 意味着顶层错误的展示文案就是内层的文案。
    assert_eq!(agent_error.to_string(), "[tool not found]: nope");
}
