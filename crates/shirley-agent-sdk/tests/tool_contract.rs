//! Regression tests for the tool-layer error contract.
//!
//! These lock this promise: **a tool author only needs to return `ToolError` and does not need to be
//! aware of `AgentError`.**
//!
//! Before the refactor, `ToolFuture`'s error type was `AgentError`, and the macro even wrote
//! `map_err(::shirley_agent_sdk::AgentError::Tool)?`, which forced every tool author to reference the
//! SDK's top-level error type.
//! Now the tool layer only exposes `ToolError`, and the runtime converges it at the call site.

use shirley_agent_sdk::{ToolCall, ToolContext, ToolError, ToolManager, tool};

/// Construct a single tool call.
///
/// `ToolCall` is a standalone struct, not a variant of `Message` — it only describes "which tool the
/// model asks to call
/// and with what arguments", decoupled from the message stream, so it is constructed directly here
/// rather than hung on `Message`.
fn tool_call(id: &str, name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

/// The most ordinary tool: the success path.
/// Note the signature has only `ToolError`, no `AgentError`.
#[tool(description = "add two numbers")]
async fn add(
    #[param(description = "left operand")] left: i64,
    #[param(description = "right operand")] right: i64,
) -> Result<i64, ToolError> {
    Ok(left + right)
}

/// A tool that fails: verifies the error passes through the macro and ToolManager unchanged.
#[tool(description = "always fails")]
async fn always_fails(
    #[param(description = "failure message")] message: String,
) -> Result<String, ToolError> {
    Err(ToolError::ExecutionError(message))
}

/// The full chain of tool registration, invocation, and argument deserialization.
#[tokio::test]
async fn tool_round_trip() {
    let mut manager = ToolManager::new();
    manager.register(add::tool()).expect("registration should succeed");

    let call = tool_call("call_1", "add", r#"{"left": 2, "right": 3}"#);
    let output = manager.invoke(&call, ToolContext::new()).await.expect("invocation should succeed");

    assert_eq!(output, serde_json::json!(5));
}

/// An error returned by a tool must pass through the macro, with the correct classification.
#[tokio::test]
async fn tool_error_passes_through() {
    use shirley_agent_sdk::SdkError;

    let mut manager = ToolManager::new();
    manager.register(always_fails::tool()).expect("registration should succeed");

    let call = tool_call("call_1", "always_fails", r#"{"message": "boom"}"#);
    let error = manager.invoke(&call, ToolContext::new()).await.expect_err("should fail");

    // The ExecutionError the tool throws itself must be preserved as-is, not wrapped into something else.
    assert!(
        matches!(error, ToolError::ExecutionError(ref m) if m == "boom"),
        "the error should pass through the macro unchanged, got: {error:?}"
    );
    // The classification must also be right: a tool execution failure means retrying the same arguments is pointless.
    assert_eq!(error.kind(), shirley_agent_sdk::ErrorKind::ToolFailure);
    assert!(!error.is_retryable());
}

/// Invalid arguments should be `ArgumentsError`, not an execution failure.
#[tokio::test]
async fn bad_arguments_are_reported_as_arguments_error() {
    let mut manager = ToolManager::new();
    manager.register(add::tool()).expect("registration should succeed");

    // missing field
    let call = tool_call("call_1", "add", r#"{"left": 2}"#);
    let error = manager.invoke(&call, ToolContext::new()).await.expect_err("missing field should fail");
    assert!(
        matches!(error, ToolError::ArgumentsError(_)),
        "a missing field should be ArgumentsError, got: {error:?}"
    );

    // arguments is not JSON at all
    let call = tool_call("call_2", "add", "not json");
    let error = manager.invoke(&call, ToolContext::new()).await.expect_err("invalid JSON should fail");
    assert!(
        matches!(error, ToolError::ArgumentsError(_)),
        "invalid JSON should be ArgumentsError, got: {error:?}"
    );
}

/// Calling a nonexistent tool should be `NotFoundError`.
#[tokio::test]
async fn unknown_tool_is_not_found() {
    let manager = ToolManager::new();
    let call = tool_call("call_1", "nope", "{}");
    let error = manager.invoke(&call, ToolContext::new()).await.expect_err("a nonexistent tool should fail");
    assert!(
        matches!(error, ToolError::NotFoundError(_)),
        "should be NotFoundError, got: {error:?}"
    );
}

/// Duplicate registration should be `RepetitionError`.
#[test]
fn duplicate_registration_is_rejected() {
    let mut manager = ToolManager::new();
    manager.register(add::tool()).expect("first registration should succeed");
    let error = manager.register(add::tool()).expect_err("duplicate registration should fail");
    assert!(
        matches!(error, ToolError::RepetitionError(_)),
        "should be RepetitionError, got: {error:?}"
    );
}

/// Tool-layer errors must converge automatically into the top-level error (`#[from]` works).
#[test]
fn tool_error_converges_into_agent_error() {
    use shirley_agent_sdk::AgentError;

    let agent_error: AgentError = ToolError::NotFoundError("nope".into()).into();
    // `transparent` means the top-level error's display text is the inner one's text.
    assert_eq!(agent_error.to_string(), "[tool not found]: nope");
}
