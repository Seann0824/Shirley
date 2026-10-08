mod app;
mod command;
pub mod desktop;
mod event;
mod markdown;
mod selection;
mod tui;
mod ui;
mod update;

use crate::bootstrap::Bootstrap;

/// 启动 ratatui TUI。
pub async fn run(bootstrap: Bootstrap) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = tui::run(&mut terminal, bootstrap).await;
    ratatui::restore();
    result
}
