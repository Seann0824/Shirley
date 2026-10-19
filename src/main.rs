use shirley_agent_sdk::AgentError;

mod bootstrap;
mod interface;
mod memory;
mod models;
mod model_providers;
mod project;
mod remote;
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

fn main() -> Result<(), AgentError> {
    dotenvy::dotenv().ok();

    // All interfaces (desktop / TUI / cloud server) share one state root: `~/.shirley`.
    // Running them on the same machine under the same OS user intentionally means they
    // see the same projects / sessions / config. Move the legacy `<config_dir>/shirley`
    // root over once so the desktop's existing data does not appear lost.
    if storage::migrate_legacy_root()
        .map_err(|error| AgentError::Other(Box::new(error)))?
    {
        eprintln!(
            "[storage] migrated legacy state root into {}",
            storage::global_root().display()
        );
    }

    // Workspace root: decides "which project this run is for", and is the source of workspace-level config.
    let working_dir = prompt::workspace_root();

    // Move legacy project-local state before any session files are opened.
    let migrated = storage::migrate_all(&working_dir)
        .map_err(|error| AgentError::Other(Box::new(error)))?;
    if std::env::args().any(|arg| arg == "--migrate-data") {
        println!("Migrated {migrated} legacy workspaces into {}", storage::global_root().display());
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| AgentError::Other(Box::new(error)))?;

    let factory = runtime.block_on(async {
        // Assembly and catalog prewarming may spawn background tasks.
        let factory = bootstrap::AgentFactory::assemble(working_dir)?;
        factory.prewarm_model_catalog();
        // A local offline copy is used by every interface. The worker shares only personal entries.
        remote::start_sync_worker(storage::global_root());
        Ok::<_, AgentError>(factory)
    })?;

    match Mode::from_env_and_args() {
        Mode::Tui => runtime.block_on(interface::run(factory)),
        // Desktop interface: enter the Tauri event loop (blocks the main thread).
        Mode::Desktop => {
            // Share the live runtime, but enter Tauri outside any block_on.
            #[cfg(feature = "desktop")]
            tauri::async_runtime::set(runtime.handle().clone());
            interface::desktop::run(factory)
        }
        // Cloud web interface: run the HTTP + WebSocket server on the tokio runtime.
        #[cfg(feature = "server")]
        Mode::Server => runtime.block_on(interface::server::run(factory)),
        #[cfg(not(feature = "server"))]
        Mode::Server => Err(std::io::Error::other(
            "server mode requires building with `--features server`",
        )),
    }
    .map_err(|error| AgentError::Other(Box::new(error)))?;

    Ok(())
}
