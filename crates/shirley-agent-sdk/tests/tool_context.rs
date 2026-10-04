//! 工具运行时上下文注入的回归测试（`docs/sdk-gaps.md` gap-1）。
//!
//! 锁的是这条承诺：**有状态工具也能用 `#[tool]` 宏**——工具从 `ToolContext`
//! 取出应用注入的句柄（这里是 `Arc<Mutex<...>>`），读写自己的状态，
//! 且并发调用互不干扰。

use std::sync::{Arc, Mutex};

use shirley_agent_sdk::{ToolCall, ToolContext, ToolError, ToolManager, tool};

/// 应用侧状态：一个累加计数。真实场景里这里会是 session / 文件句柄等。
#[derive(Default)]
struct Session {
    counter: u32,
}

/// 一个有状态工具：从上下文取 session，读改写计数。
///
/// 用 `&ToolContext` 引用形式——验证宏对引用声明的处理。
#[tool(description = "把计数加一")]
async fn bump(
    ctx: &ToolContext,
    #[param(description = "增量")] by: u32,
) -> Result<u32, ToolError> {
    let session = ctx
        .get::<Arc<Mutex<Session>>>()
        .ok_or_else(|| ToolError::ExecutionError("session not provided".into()))?;
    let mut guard = session.lock().unwrap();
    guard.counter += by;
    Ok(guard.counter)
}

/// 一个无上下文工具：验证「没有 ctx 参数的旧写法」不受影响。
#[tool(description = "恒等")]
async fn identity(#[param(description = "值")] value: u32) -> Result<u32, ToolError> {
    Ok(value)
}

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

/// 上下文能穿过宏被工具读到，且状态可读写。
#[tokio::test]
async fn context_is_injected_into_macro_tool() {
    let session = Arc::new(Mutex::new(Session::default()));
    let ctx = ToolContext::new().with(session.clone());

    let mut manager = ToolManager::new();
    manager.register(bump::tool()).expect("注册应成功");

    let out = manager
        .invoke(&call("bump", r#"{"by": 2}"#), ctx.clone())
        .await
        .expect("调用应成功");
    assert_eq!(out, serde_json::json!(2));

    let out = manager
        .invoke(&call("bump", r#"{"by": 3}"#), ctx)
        .await
        .expect("调用应成功");
    assert_eq!(out, serde_json::json!(5));
    assert_eq!(session.lock().unwrap().counter, 5);
}

/// 并发调用同一份上下文：互斥锁保证累加不丢。
#[tokio::test]
async fn context_is_shared_across_concurrent_calls() {
    let session = Arc::new(Mutex::new(Session::default()));
    let ctx = ToolContext::new().with(session.clone());

    let mut manager = ToolManager::new();
    manager.register(bump::tool()).expect("注册应成功");

    let mut tasks = Vec::new();
    for _ in 0..10 {
        let manager = &manager;
        let ctx = ctx.clone();
        tasks.push(async move {
            manager
                .invoke(&call("bump", r#"{"by": 1}"#), ctx)
                .await
                .expect("调用应成功")
        });
    }
    futures::future::join_all(tasks).await;

    assert_eq!(session.lock().unwrap().counter, 10);
}

/// 工具声明了 ctx 但应用没注入：工具自己返回错误，而不是 panic。
#[tokio::test]
async fn missing_context_surfaces_as_tool_error() {
    let mut manager = ToolManager::new();
    manager.register(bump::tool()).expect("注册应成功");

    let error = manager
        .invoke(&call("bump", r#"{"by": 1}"#), ToolContext::new())
        .await
        .expect_err("缺上下文应失败");
    assert!(matches!(error, ToolError::ExecutionError(_)), "{error:?}");
}

/// ctx 参数不能出现在生成的参数 Schema 里（否则模型会尝试填它）。
#[test]
fn context_parameter_is_not_in_schema() {
    let definition = bump::definition();
    let properties = &definition.parameters["properties"];
    assert!(
        properties.get("ctx").is_none(),
        "ctx 不该出现在 schema: {properties}"
    );
    assert!(properties.get("by").is_some(), "普通参数应保留: {properties}");
}

/// 不带 ctx 的旧式工具照常工作，且其 schema 不受上下文影响。
#[tokio::test]
async fn tools_without_context_still_work() {
    let mut manager = ToolManager::new();
    manager.register(identity::tool()).expect("注册应成功");

    let out = manager
        .invoke(&call("identity", r#"{"value": 7}"#), ToolContext::new())
        .await
        .expect("调用应成功");
    assert_eq!(out, serde_json::json!(7));
}
