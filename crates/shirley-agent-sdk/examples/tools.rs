//! The tool system: a stateless tool, a stateful tool that reads `ToolContext`, and the
//! `on_register` / `on_unregister` lifecycle hooks.
//!
//! Registration and dependency injection happen in the same step (`on_register` writes the
//! shared state into the context), so they cannot drift apart.

use std::sync::{Arc, Mutex};

use shirley_agent_sdk::{ToolContext, ToolError, ToolManager, tool};

/// A stateless tool: the success path. Every parameter needs a `#[param(description = ...)]`.
#[tool(description = "Add two integers")]
async fn add(
    #[param(description = "Left operand")] left: i64,
    #[param(description = "Right operand")] right: i64,
) -> Result<i64, ToolError> {
    Ok(left + right)
}

/// Shared state a stateful tool needs. Give it its own newtype so `ctx.remove::<T>()` cannot
/// delete an unrelated value of the same underlying type.
#[derive(Default)]
struct Counter {
    value: i64,
}

/// A `ToolContext` (or `&ToolContext`) parameter is injected by the SDK and never appears in
/// the generated JSON Schema.
#[tool(description = "Increment a shared counter and return the new value")]
async fn bump(
    ctx: &ToolContext,
    #[param(description = "Amount to add")] by: i64,
) -> Result<i64, ToolError> {
    let counter = ctx
        .get::<Arc<Mutex<Counter>>>()
        .ok_or_else(|| ToolError::ExecutionError("counter not provided".into()))?;
    let mut guard = counter.lock().unwrap();
    guard.value += by;
    Ok(guard.value)
}

/// Injected at registration time (the context is handed to the tool).
fn bump_on_register(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.insert(Arc::new(Mutex::new(Counter::default())));
    Ok(())
}

/// Released when the tool is unregistered.
fn bump_on_unregister(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.remove::<Arc<Mutex<Counter>>>();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut manager = ToolManager::new();
    manager.register(add::tool())?;
    // Hooks are wired by the macro; you only write the functions.
    manager.register(bump_with_hooks::tool())?;

    // A tool's `definitions()` are sorted by name so the prefix stays stable (KV cache).
    for definition in manager.definitions() {
        println!("tool: {} — {}", definition.name, definition.description);
    }

    // Invoke directly (no model involved): what the runtime does after a tool call.
    let ctx = manager.context().clone();
    let call = shirley_agent_sdk::ToolCall {
        id: "call_1".into(),
        name: "add".into(),
        arguments: r#"{"left": 2, "right": 3}"#.into(),
    };
    let output = manager.invoke(&call, ctx.clone()).await?;
    println!("add -> {output}");

    let call = shirley_agent_sdk::ToolCall {
        id: "call_2".into(),
        name: "bump".into(),
        arguments: r#"{"by": 5}"#.into(),
    };
    println!("bump -> {}", manager.invoke(&call, ctx).await?);

    // Unregistering runs `on_unregister` (best-effort cleanup).
    manager.unregister("bump")?;
    Ok(())
}

/// `#[tool]` with hooks: `on_register` / `on_unregister` reference plain functions defined in
/// the same module as the tool function.
#[tool(
    description = "Increment a shared counter and return the new value",
    on_register = bump_on_register,
    on_unregister = bump_on_unregister
)]
async fn bump_with_hooks(
    ctx: &ToolContext,
    #[param(description = "Amount to add")] by: i64,
) -> Result<i64, ToolError> {
    bump(ctx, by).await
}
