use shirley_agent_sdk::AgentError;

mod bootstrap;
mod interface;
mod models;
mod prompt;
mod session;
mod settings;
mod tools;
mod workspace_search;

/// 界面模式：TUI（默认）或桌面界面。
///
/// 通过启动参数 `--desktop` 选择，环境变量 `SHIRLEY_INTERFACE=desktop` 亦可。
/// 两个界面共享同一份 [`bootstrap::Bootstrap`] 装配产物（见 `docs/desktop-interface.md`）。
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

    // 工作区根目录：决定"这次在哪个项目跑"，也是工作区级配置的来源。
    let working_dir = prompt::workspace_root();

    // 应用装配（配置 / 模型目录 / 工具 / 会话 / Agent）——两个界面共用。
    let bootstrap = bootstrap::Bootstrap::assemble(working_dir)?;

    match Mode::from_env_and_args() {
        Mode::Tui => interface::run(bootstrap).await,
        // 桌面界面：进入 Tauri 事件循环（阻塞主线程）。
        Mode::Desktop => interface::desktop::run(bootstrap),
    }
    .map_err(|error| AgentError::Other(Box::new(error)))?;

    Ok(())
}
