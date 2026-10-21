pub mod desktop;
#[cfg(any(feature = "desktop", feature = "server"))]
pub(crate) mod runtime;
#[cfg(feature = "server")]
pub mod server;
pub(crate) mod session;
mod tui;
pub(crate) mod view;

use std::sync::Arc;

use crate::bootstrap::AgentFactory;

use self::session::SessionManager;

/// Start the ratatui TUI.
///
/// The assembly product is [`AgentFactory`] (the multi-session factory, `docs/multi-session.md`
/// decision 5): here we build the first `Agent` under the existing "open an empty session on
/// launch" UX, put it into [`SessionManager`] along with the factory, and each session builds
/// its own agent afterwards.
pub async fn run(factory: AgentFactory) -> std::io::Result<()> {
    // Lazily open an empty session at startup (only names it here; nothing hits disk until the
    // first message). Session persistence lives in the app layer: we load an empty working set,
    // hand it to the factory to build the `Agent`, and pass the store along to `SessionManager` —
    // persistence happens through its event-driven path.
    let session_catalog = Arc::clone(&factory.session_catalog);
    let (entry, store) = session_catalog
        .create_lazy(None)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let log = store
        .load()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let model_catalog = Arc::clone(&factory.model_catalog);
    let needs_login = factory.needs_login;
    let factory = Arc::new(factory);
    let built = factory
        .build_agent(log)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let sessions = SessionManager::with_factory(
        built.agent,
        Some(entry.name),
        session_catalog,
        factory,
        Some(store),
        Some(built.memory),
        Some(built.skills),
        Some(built.todo),
        built.subagents,
        Some(built.approval),
    );

    let mut terminal = ratatui::init();
    let result = tui::run(&mut terminal, sessions, model_catalog, needs_login).await;
    ratatui::restore();
    result
}
