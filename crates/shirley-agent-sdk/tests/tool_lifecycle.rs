//! Public contract tests for the tool lifecycle (created / executed / destroy) (`docs/tool-lifecycle.md`).
//!
//! These lock these promises:
//!
//! 1. `register` calls `on_register` (= created) after the duplicate check and before insertion; if the
//!    hook errors, the tool does not enter the table;
//! 2. `unregister` first moves the tool out of the table, then calls `on_unregister` (= destroy); an
//!    unregistered name returns `NotFoundError`;
//! 3. `ToolContext` is owned by `ToolManager`, and hooks can inject / remove data in it;
//! 4. The `#[tool]` function macro can declare companion hooks with `on_register = ...` / `on_unregister = ...`;
//! 5. Stateless tools need zero changes — with no hooks written, they register / invoke / unregister as usual.
//!
//! Relies only on the public API of `shirley_agent_sdk`.

use std::sync::atomic::{AtomicUsize, Ordering};

use shirley_agent_sdk::{ToolCall, ToolContext, ToolError, ToolManager, tool};

/// Private data for a stateful tool (a dedicated newtype, to avoid colliding TypeId with other tools).
struct CounterState {
    value: AtomicUsize,
}

/// Global probes counting hook invocations (test-only; real tools use newtype state).
static REGISTER_CALLS: AtomicUsize = AtomicUsize::new(0);
static UNREGISTER_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Register hook: injects state and records the call.
fn bump_on_register(ctx: &mut ToolContext) -> Result<(), ToolError> {
    REGISTER_CALLS.fetch_add(1, Ordering::SeqCst);
    ctx.insert(CounterState {
        value: AtomicUsize::new(0),
    });
    Ok(())
}

/// Unregister hook: clears the state it injected and records the call.
fn bump_on_unregister(ctx: &mut ToolContext) -> Result<(), ToolError> {
    UNREGISTER_CALLS.fetch_add(1, Ordering::SeqCst);
    ctx.remove::<CounterState>();
    Ok(())
}

/// A stateful tool using companion hooks.
#[tool(
    description = "increment the counter",
    on_register = bump_on_register,
    on_unregister = bump_on_unregister,
)]
async fn bump(
    ctx: &ToolContext,
    #[param(description = "increment")] by: u64,
) -> Result<u64, ToolError> {
    let state = ctx
        .get::<CounterState>()
        .ok_or_else(|| ToolError::ExecutionError("state not provided".into()))?;
    let next = state.value.fetch_add(by as usize, Ordering::SeqCst) + by as usize;
    Ok(next as u64)
}

/// A stateless tool: writes no hooks, verifying backward compatibility.
#[tool(description = "identity")]
async fn identity(#[param(description = "value")] value: u32) -> Result<u32, ToolError> {
    Ok(value)
}

/// A register hook that deliberately fails.
fn failing(_ctx: &mut ToolContext) -> Result<(), ToolError> {
    Err(ToolError::ExecutionError("missing config".into()))
}

/// A tool whose registration always fails (the macro-generated `mod broken` is at the same level as the
/// function, so it must be module-level).
#[tool(description = "always fails to register", on_register = failing)]
async fn broken(#[param(description = "value")] value: u32) -> Result<u32, ToolError> {
    Ok(value)
}

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

/// register triggers the created hook; the state the hook injected can be read by the tool.
#[tokio::test]
async fn register_runs_on_register_and_injects_state() {
    REGISTER_CALLS.store(0, Ordering::SeqCst);

    let mut manager = ToolManager::new();
    manager.register(bump::tool()).expect("registration should succeed");

    assert_eq!(REGISTER_CALLS.load(Ordering::SeqCst), 1, "created should be called once");
    assert!(
        manager.context().get::<CounterState>().is_some(),
        "the state injected by the hook should be in the manager context"
    );

    // executed: the state reaches the tool through the context and is accumulated.
    let out = manager
        .invoke(&call("bump", r#"{"by": 2}"#), manager.context().clone())
        .await
        .expect("invocation should succeed");
    assert_eq!(out, serde_json::json!(2));
}

/// unregister triggers the destroy hook; the hook can clean up the context entry it injected.
#[test]
fn unregister_runs_on_unregister_and_cleans_context() {
    UNREGISTER_CALLS.store(0, Ordering::SeqCst);

    let mut manager = ToolManager::new();
    manager.register(bump::tool()).expect("registration should succeed");
    assert!(manager.context().get::<CounterState>().is_some());

    manager.unregister("bump").expect("unregister should succeed");

    assert_eq!(UNREGISTER_CALLS.load(Ordering::SeqCst), 1, "destroy should be called once");
    assert!(
        manager.context().get::<CounterState>().is_none(),
        "destroy should clear the context entry it injected"
    );
    // The tool has been removed from the table: a further call should be NotFound.
    assert!(manager.definitions().iter().all(|d| d.name != "bump"));
}

/// Unregistering an unregistered tool returns NotFoundError.
#[test]
fn unregister_unknown_tool_is_not_found() {
    let mut manager = ToolManager::new();
    let error = manager.unregister("nope").expect_err("unregistering an unregistered tool should fail");
    assert!(
        matches!(error, ToolError::NotFoundError(_)),
        "should be NotFoundError, got: {error:?}"
    );
}

/// A created-hook error -> registration fails and the tool does not enter the table (no half-registered state).
#[test]
fn on_register_failure_rejects_registration() {
    let mut manager = ToolManager::new();
    let error = manager.register(broken::tool()).expect_err("a failing hook should reject registration");
    assert!(matches!(error, ToolError::ExecutionError(_)), "{error:?}");
    assert!(
        manager.definitions().iter().all(|d| d.name != "broken"),
        "a tool whose registration failed should not enter the table"
    );
}

/// A stateless tool (no hooks) needs zero changes: register, invoke, unregister all as usual.
#[tokio::test]
async fn stateless_tool_without_hooks_still_works() {
    let mut manager = ToolManager::new();
    manager.register(identity::tool()).expect("registration should succeed");

    let out = manager
        .invoke(&call("identity", r#"{"value": 7}"#), ToolContext::new())
        .await
        .expect("the call should succeed");
    assert_eq!(out, serde_json::json!(7));

    manager.unregister("identity").expect("unregistration should succeed");
}
