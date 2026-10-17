use shirley_agent_sdk::AgentError;

mod bootstrap;
mod interface;
mod memory;
mod models;
mod project;
mod prompt;
mod prompt_template;
mod session;
mod settings;
mod storage;
mod subagent;
mod skills;
mod todo;
mod tools;
mod workspace_search;

/// Interface mode: TUI (default) or the desktop interface.
///
/// Selected by the `--desktop` launch flag; the `SHIRLEY_INTERFACE=desktop` env var works too.
/// Both interfaces share the same [`bootstrap::AgentFactory`] assembly (see `docs/desktop-interface.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Tui,
    Desktop,
    /// Cloud web interface: HTTP + WebSocket server, only-chat UI (no project
    /// management). Gated behind the `server` cargo feature (`--features server`).
    Server,
}

impl Mode {
    fn from_env_and_args() -> Self {
        let interface = std::env::var("SHIRLEY_INTERFACE").ok();
        if std::env::args().any(|arg| arg == "--server") || interface.as_deref() == Some("server") {
            Mode::Server
        } else if std::env::args().any(|arg| arg == "--desktop")
            || interface.as_deref() == Some("desktop")
        {
            Mode::Desktop
        } else {
            Mode::Tui
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), AgentError> {
    dotenvy::dotenv().ok();

    // Workspace root: decides "which project this run is for", and is the source of workspace-level config.
    let working_dir = prompt::workspace_root();

    // Move legacy project-local state before any session files are opened.
    let migrated = storage::migrate_all(&working_dir)
        .map_err(|error| AgentError::Other(Box::new(error)))?;
    if std::env::args().any(|arg| arg == "--migrate-data") {
        println!("Migrated {migrated} legacy workspaces into {}", storage::global_root().display());
        return Ok(());
    }

    // Application assembly (config / model catalog / tools / sessions / Agent) — shared by both interfaces.
    let factory = bootstrap::AgentFactory::assemble(working_dir)?;

    // Warm the model catalog in the background so the first `/model` open doesn't wait on a network round trip.
    factory.prewarm_model_catalog();

    match Mode::from_env_and_args() {
        Mode::Tui => interface::run(factory).await,
        // Desktop interface: enter the Tauri event loop (blocks the main thread).
        Mode::Desktop => interface::desktop::run(factory),
        // Cloud web interface: run the HTTP + WebSocket server on the tokio runtime.
        #[cfg(feature = "server")]
        Mode::Server => interface::server::run(factory).await,
        #[cfg(not(feature = "server"))]
        Mode::Server => Err(std::io::Error::other(
            "server mode requires building with `--features server`",
        )),
    }
    .map_err(|error| AgentError::Other(Box::new(error)))?;

    Ok(())
}
