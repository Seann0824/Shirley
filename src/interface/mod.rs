mod app;
mod command;
mod event;
mod markdown;
mod tui;
mod ui;
mod update;

pub async fn run(
    agent: agent_sdk::Agent,
    catalog: std::sync::Arc<dyn crate::models::ModelCatalog>,
) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = tui::run(&mut terminal, agent, catalog).await;
    ratatui::restore();
    result
}
