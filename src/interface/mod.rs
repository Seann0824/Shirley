mod app;
mod command;
pub mod desktop;
mod event;
mod markdown;
mod selection;
pub(crate) mod session;
mod tui;
mod ui;
mod update;

use std::sync::Arc;

use crate::bootstrap::AgentFactory;

use self::session::SessionManager;

/// 启动 ratatui TUI。
///
/// 装配产物是 [`AgentFactory`]（多会话工厂，`docs/multi-session.md` 决策 5）：
/// 这里先按「启动即开一份空会话」的既有 UX 造出首个 `Agent`，连同工厂一起塞进
/// [`SessionManager`]，之后每个会话各自 `build_agent`。
pub async fn run(factory: AgentFactory) -> std::io::Result<()> {
    // 启动时惰性开一份空会话（此刻只定名、不落盘，发首条消息才建文件）。
    // 会话持久化在应用层：这里 load 得空工作集交给工厂起 `Agent`，store 一并交给
    // `SessionManager`——落库由其事件驱动路径完成。
    let session_catalog = factory.session_catalog.clone();
    let (entry, store) = session_catalog
        .create_lazy(None)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let log = store
        .load()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let built = factory
        .build_agent(log)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let model_catalog = factory.model_catalog.clone();
    let needs_login = factory.needs_login;
    let sessions = SessionManager::with_factory(
        built.agent,
        Some(entry.name),
        session_catalog,
        Arc::new(factory),
        Some(store),
        Some(built.memory),
    );

    let mut terminal = ratatui::init();
    let result = tui::run(&mut terminal, sessions, model_catalog, needs_login).await;
    ratatui::restore();
    result
}
