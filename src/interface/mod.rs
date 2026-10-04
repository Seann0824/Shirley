mod app;
mod command;
mod event;
mod markdown;
mod selection;
mod tui;
mod ui;
mod update;

pub async fn run(
    agent: shirley_agent_sdk::Agent,
    catalog: std::sync::Arc<dyn crate::models::ModelCatalog>,
    session_catalog: std::sync::Arc<dyn crate::session::SessionCatalog>,
    current_session: Option<String>,
    needs_login: bool,
) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = tui::run(
        &mut terminal,
        agent,
        catalog,
        session_catalog,
        current_session,
        needs_login,
    )
    .await;
    ratatui::restore();
    result
}
