//! 桌面界面（Tauri 2 + React）——见 `docs/desktop-interface.md`。
//!
//! 与 TUI 是**兄弟界面**：共享同一份 [`crate::bootstrap::AgentFactory`]（同一个
//! 模型配置 / 工具 / 会话目录），只是渲染方式不同。desktop 侧用 Tauri command 驱动
//! 每个会话各自的 `Agent::run_stream`，把 `AgentEvent` 经 [`wire`] 翻译成 webview 事件推给前端。
//!
//! 编译门槛：只有启用 `desktop` feature 时才编译 Tauri 壳，默认（TUI-only）
//! 构建不触碰 webview 工具链。前端产物由 `web/`（Vite）构建到 `web/dist`。

pub mod wire;

use crate::bootstrap::AgentFactory;

#[cfg(feature = "desktop")]
mod shell;

/// 启动桌面界面（阻塞：进入 Tauri 事件循环，需在主线程调用）。
#[cfg(feature = "desktop")]
pub fn run(factory: AgentFactory) -> std::io::Result<()> {
    shell::run(factory)
}

/// 未启用 `desktop` feature 时的兜底：如实报错，不静默降级。
#[cfg(not(feature = "desktop"))]
pub fn run(_factory: AgentFactory) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "桌面界面未编译；请用 `cargo run --features desktop -- --desktop` 构建（见 docs/desktop-interface.md）",
    ))
}
