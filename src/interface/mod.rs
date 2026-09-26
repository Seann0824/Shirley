mod app;
mod event;
mod tui;
mod ui;
mod update;

pub async fn run(agent: agent_sdk::Agent) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = tui::run(&mut terminal, agent).await;
    ratatui::restore();
    result
}
