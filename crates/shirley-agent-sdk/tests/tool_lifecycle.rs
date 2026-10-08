//! 工具生命周期（created / executed / destroy）的公开契约测试（`docs/tool-lifecycle.md`）。
//!
//! 锁的是这几条承诺：
//!
//! 1. `register` 在查重通过后、插入前调用 `on_register`（= created）；钩子报错则工具不入表；
//! 2. `unregister` 先把工具移出表、再调用 `on_unregister`（= destroy），未注册返回 `NotFoundError`；
//! 3. `ToolContext` 的所有权在 `ToolManager` 内，钩子能向其中注入 / 移除数据；
//! 4. `#[tool]` 函数宏可用 `on_register = ...` / `on_unregister = ...` 声明伴生钩子；
//! 5. 无状态工具零改动——不写钩子照常注册 / 调用 / 注销。
//!
//! 只依赖 `shirley_agent_sdk` 的公开 API。

use std::sync::atomic::{AtomicUsize, Ordering};

use shirley_agent_sdk::{ToolCall, ToolContext, ToolError, ToolManager, tool};

/// 有状态工具私有的一份数据（用独立 newtype，避免与别的工具撞 TypeId）。
struct CounterState {
    value: AtomicUsize,
}

/// 记录钩子调用次数的全局探针（仅测试用；真实工具用 newtype 状态）。
static REGISTER_CALLS: AtomicUsize = AtomicUsize::new(0);
static UNREGISTER_CALLS: AtomicUsize = AtomicUsize::new(0);

/// 注册钩子：注入状态，并记录被调用。
fn bump_on_register(ctx: &mut ToolContext) -> Result<(), ToolError> {
    REGISTER_CALLS.fetch_add(1, Ordering::SeqCst);
    ctx.insert(CounterState {
        value: AtomicUsize::new(0),
    });
    Ok(())
}

/// 注销钩子：清掉自己注入的状态，并记录被调用。
fn bump_on_unregister(ctx: &mut ToolContext) -> Result<(), ToolError> {
    UNREGISTER_CALLS.fetch_add(1, Ordering::SeqCst);
    ctx.remove::<CounterState>();
    Ok(())
}

/// 一个用伴生钩子的有状态工具。
#[tool(
    description = "把计数加一",
    on_register = bump_on_register,
    on_unregister = bump_on_unregister,
)]
async fn bump(
    ctx: &ToolContext,
    #[param(description = "增量")] by: u64,
) -> Result<u64, ToolError> {
    let state = ctx
        .get::<CounterState>()
        .ok_or_else(|| ToolError::ExecutionError("state not provided".into()))?;
    let next = state.value.fetch_add(by as usize, Ordering::SeqCst) + by as usize;
    Ok(next as u64)
}

/// 一个无状态工具：不写任何钩子，验证向后兼容。
#[tool(description = "恒等")]
async fn identity(#[param(description = "值")] value: u32) -> Result<u32, ToolError> {
    Ok(value)
}

/// 故意失败的注册钩子。
fn failing(_ctx: &mut ToolContext) -> Result<(), ToolError> {
    Err(ToolError::ExecutionError("配置缺失".into()))
}

/// 一个注册必定失败的工具（宏生成的 `mod broken` 与函数同层，故须放模块级）。
#[tool(description = "永远注册失败", on_register = failing)]
async fn broken(#[param(description = "值")] value: u32) -> Result<u32, ToolError> {
    Ok(value)
}

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

/// register 触发 created 钩子；钩子注入的状态能被工具读到。
#[tokio::test]
async fn register_runs_on_register_and_injects_state() {
    REGISTER_CALLS.store(0, Ordering::SeqCst);

    let mut manager = ToolManager::new();
    manager.register(bump::tool()).expect("注册应成功");

    assert_eq!(REGISTER_CALLS.load(Ordering::SeqCst), 1, "created 应被调用一次");
    assert!(
        manager.context().get::<CounterState>().is_some(),
        "钩子注入的状态应在 manager 上下文里"
    );

    // executed：状态穿过上下文被工具读到并累加。
    let out = manager
        .invoke(&call("bump", r#"{"by": 2}"#), manager.context().clone())
        .await
        .expect("调用应成功");
    assert_eq!(out, serde_json::json!(2));
}

/// unregister 触发 destroy 钩子；钩子能清理自己注入的上下文项。
#[test]
fn unregister_runs_on_unregister_and_cleans_context() {
    UNREGISTER_CALLS.store(0, Ordering::SeqCst);

    let mut manager = ToolManager::new();
    manager.register(bump::tool()).expect("注册应成功");
    assert!(manager.context().get::<CounterState>().is_some());

    manager.unregister("bump").expect("注销应成功");

    assert_eq!(UNREGISTER_CALLS.load(Ordering::SeqCst), 1, "destroy 应被调用一次");
    assert!(
        manager.context().get::<CounterState>().is_none(),
        "destroy 应清掉自己注入的上下文项"
    );
    // 工具已从表里移除：再调用应是 NotFound。
    assert!(manager.definitions().iter().all(|d| d.name != "bump"));
}

/// 注销未注册的工具返回 NotFoundError。
#[test]
fn unregister_unknown_tool_is_not_found() {
    let mut manager = ToolManager::new();
    let error = manager.unregister("nope").expect_err("未注册应失败");
    assert!(
        matches!(error, ToolError::NotFoundError(_)),
        "应是 NotFoundError，实际: {error:?}"
    );
}

/// created 钩子报错 → 注册失败，工具不入表（不留半注册状态）。
#[test]
fn on_register_failure_rejects_registration() {
    let mut manager = ToolManager::new();
    let error = manager.register(broken::tool()).expect_err("钩子失败应拒绝注册");
    assert!(matches!(error, ToolError::ExecutionError(_)), "{error:?}");
    assert!(
        manager.definitions().iter().all(|d| d.name != "broken"),
        "注册失败的工具不应入表"
    );
}

/// 无状态工具（无钩子）零改动：注册、调用、注销都照常。
#[tokio::test]
async fn stateless_tool_without_hooks_still_works() {
    let mut manager = ToolManager::new();
    manager.register(identity::tool()).expect("注册应成功");

    let out = manager
        .invoke(&call("identity", r#"{"value": 7}"#), ToolContext::new())
        .await
        .expect("调用应成功");
    assert_eq!(out, serde_json::json!(7));

    manager.unregister("identity").expect("注销应成功");
}
