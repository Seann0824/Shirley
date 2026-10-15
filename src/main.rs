use shirley_agent_sdk::AgentError;

mod bootstrap;
mod interface;
mod memory;
mod models;
mod plugins;
mod project;
mod prompt;
mod prompt_template;
mod session;
mod settings;
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
}

impl Mode {
    fn from_env_and_args() -> Self {
        if std::env::args().any(|arg| arg == "--desktop")
            || std::env::var("SHIRLEY_INTERFACE").as_deref() == Ok("desktop")
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

    // Application assembly (config / model catalog / tools / sessions / Agent) — shared by both interfaces.
    let factory = bootstrap::AgentFactory::assemble(working_dir)?;

    // Warm the model catalog in the background so the first `/model` open doesn't wait on a network round trip.
    factory.prewarm_model_catalog();

    match Mode::from_env_and_args() {
        Mode::Tui => interface::run(factory).await,
        // Desktop interface: enter the Tauri event loop (blocks the main thread).
        Mode::Desktop => interface::desktop::run(factory),
    }
    .map_err(|error| AgentError::Other(Box::new(error)))?;

    Ok(())
}
