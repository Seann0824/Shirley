//! Regression tests for tool runtime context injection (`docs/sdk-gaps.md` gap-1).
//!
//! These lock this promise: **stateful tools can also use the `#[tool]` macro** — the tool pulls an
//! application-injected handle out of the `ToolContext` (here `Arc<Mutex<...>>`), reads and writes
//! its own state, and concurrent calls do not interfere with each other.

use std::sync::{Arc, Mutex};

use shirley_agent_sdk::{ToolCall, ToolContext, ToolError, ToolManager, tool};

/// Application-side state: a running counter. In a real scenario this would be a session / file handle, etc.
#[derive(Default)]
struct Session {
    counter: u32,
}

/// A stateful tool: takes the session from the context, reads-modifies-writes the counter.
///
/// Uses the `&ToolContext` reference form — verifies the macro's handling of reference declarations.
#[tool(description = "increment the counter")]
async fn bump(
    ctx: &ToolContext,
    #[param(description = "increment")] by: u32,
) -> Result<u32, ToolError> {
    let session = ctx
        .get::<Arc<Mutex<Session>>>()
        .ok_or_else(|| ToolError::ExecutionError("session not provided".into()))?;
    let mut guard = session.lock().unwrap();
    guard.counter += by;
    Ok(guard.counter)
}

/// A context-free tool: verifies the "old style with no ctx parameter" is unaffected.
#[tool(description = "identity")]
async fn identity(#[param(description = "value")] value: u32) -> Result<u32, ToolError> {
    Ok(value)
}

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

/// The context reaches the tool through the macro, and its state is readable and writable.
#[tokio::test]
async fn context_is_injected_into_macro_tool() {
    let session = Arc::new(Mutex::new(Session::default()));
    let ctx = ToolContext::new().with(Arc::clone(&session));

    let mut manager = ToolManager::new();
    manager
        .register(bump::tool())
        .expect("registration should succeed");

    let out = manager
        .invoke(&call("bump", r#"{"by": 2}"#), ctx.clone())
        .await
        .expect("invocation should succeed");
    assert_eq!(out, serde_json::json!(2));

    let out = manager
        .invoke(&call("bump", r#"{"by": 3}"#), ctx)
        .await
        .expect("the call should succeed");
    assert_eq!(out, serde_json::json!(5));
    assert_eq!(session.lock().unwrap().counter, 5);
}

/// Concurrent calls over the same context: the mutex guarantees no increments are lost.
#[tokio::test]
async fn context_is_shared_across_concurrent_calls() {
    let session = Arc::new(Mutex::new(Session::default()));
    let ctx = ToolContext::new().with(Arc::clone(&session));

    let mut manager = ToolManager::new();
    manager
        .register(bump::tool())
        .expect("registration should succeed");

    let mut tasks = Vec::new();
    for _ in 0..10 {
        let manager = &manager;
        let ctx = ctx.clone();
        tasks.push(async move {
            manager
                .invoke(&call("bump", r#"{"by": 1}"#), ctx)
                .await
                .expect("the call should succeed")
        });
    }
    futures::future::join_all(tasks).await;

    assert_eq!(session.lock().unwrap().counter, 10);
}

/// The tool declares ctx but the application injected nothing: the tool returns an error itself, not a panic.
#[tokio::test]
async fn missing_context_surfaces_as_tool_error() {
    let mut manager = ToolManager::new();
    manager
        .register(bump::tool())
        .expect("registration should succeed");

    let error = manager
        .invoke(&call("bump", r#"{"by": 1}"#), ToolContext::new())
        .await
        .expect_err("missing context should fail");
    assert!(matches!(error, ToolError::ExecutionError(_)), "{error:?}");
}

/// The ctx parameter must not appear in the generated parameter Schema (otherwise the model will try to fill it).
#[test]
fn context_parameter_is_not_in_schema() {
    let definition = bump::definition();
    let properties = &definition.parameters["properties"];
    assert!(
        properties.get("ctx").is_none(),
        "ctx should not appear in the schema: {properties}"
    );
    assert!(
        properties.get("by").is_some(),
        "ordinary parameters should be kept: {properties}"
    );
}

/// Old-style tools without ctx work as usual, and their schema is unaffected by the context.
#[tokio::test]
async fn tools_without_context_still_work() {
    let mut manager = ToolManager::new();
    manager
        .register(identity::tool())
        .expect("registration should succeed");

    let out = manager
        .invoke(&call("identity", r#"{"value": 7}"#), ToolContext::new())
        .await
        .expect("the call should succeed");
    assert_eq!(out, serde_json::json!(7));
}
