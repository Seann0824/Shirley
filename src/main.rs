use std::sync::Arc;

use session::SessionCatalog as _;
use shirley_agent_sdk::{Agent, AgentError, ModelConfig, ToolContext, ToolManager};
mod interface;
mod models;
mod prompt;
mod session;
mod settings;
mod tools;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), AgentError> {
    dotenvy::dotenv().ok();

    // 工作区根目录：决定"这次在哪个项目跑"，也是工作区级配置的来源。
    // 注意它与 provider 配置是两类东西——`SHIRLEY_WORKSPACE` 刻意不进
    // `settings`（见该模块文档），这里先解析出来供两处共用。
    let working_dir = prompt::workspace_root();

    // 配置装载（方案 A）：优先级 `内置默认 < 全局 config.toml < 工作区
    // .shirley/config.toml < 环境变量`。`main.rs` 只消费结果，不再散读 env。
    //
    // 缺 `base_url` 不再阻断启动：程序照常进入 TUI，并在未配置时自动进入
    // `/login` 引导用户补齐（见 `needs_login` 与 `interface::run`）。
    let settings = settings::Settings::load_default(&working_dir)
        .map_err(|error| AgentError::Other(Box::new(error)))?;
    let needs_login = !settings.is_configured();

    // 模型目录：默认从 chat completions 的 base_url 推导 `/v1/models` 接口，
    // 也可用配置里的 `models_url`（或旧环境变量 `LOCAL_MODELS_URL`）显式覆盖。
    // 拉取失败时回退到内置静态列表，保证 `/model` 在远端抖动时仍可用。
    let models_url = settings
        .models_url
        .clone()
        .unwrap_or_else(|| models::models_endpoint(&settings.base_url));
    let catalog: Arc<dyn models::ModelCatalog> = Arc::new(models::RemoteCatalog::new(
        models_url,
        settings.api_key.clone(),
        models::StaticCatalog::builtin().entries(),
    ));

    // 定义一个工具Tool
    let mut tool_manager = ToolManager::new();
    let _ = tool_manager.register(tools::bash_tool::tool());
    let _ = tool_manager.register(tools::read_file_tool::tool());

    // 联网搜索（`docs/web-search.md`）：走 DeepSeek 的 Anthropic-compatible
    // Messages API，服务端工具 `web_search_20250305`。未配置 `DEEPSEEK_API_KEY`
    // 或显式关闭时不注册——模型不会看到一个永远失败的工具。
    //
    // 工具本身无状态（`#[tool]` 宏），运行所需的 client / 凭据 / 配置由
    // `WebSearchState` 承载，经 `ToolContext` 注入。未配置时既不注册工具、
    // 也不注入状态。
    let mut tool_context = ToolContext::new();
    match tools::WebSearchState::from_env() {
        Ok(Some(state)) => {
            let _ = tool_manager.register(tools::web_search_tool::tool());
            tool_context = tool_context.with(state);
        }
        Ok(None) => {}
        Err(error) => eprintln!("[web_search] 未启用：{error}"),
    }

    // 调用返回 Future；await 等待它执行完成。
    let mut model_config = ModelConfig::builder()
        .protocol(settings.protocol)
        .base_url(settings.base_url.clone())
        .maybe_api_key(settings.api_key.clone())
        .model(settings.model.clone())
        .stream(true)
        .thinking(true)
        .reasoning_effort("low")
        .build();
    model_config.context_window_tokens = Some(settings.context_window_tokens);

    // 会话持久化（`docs/session.md`）：把原始 Message 全量日志落到工作区。
    // 多会话布局：每份会话是 `.shirley/sessions/<name>.jsonl`（见 `session` 模块）。
    // 旧的单文件日志 `.shirley/session.jsonl` 会在首次启动时被收编为一份会话，
    // 保证升级不丢历史。
    //
    // 启动时**直接开一份新会话**（而不是恢复"最近修改"的旧会话）：用户跑 coding
    // agent 的起点应是一段干净的新对话，避免一上来就背上历史会话的上下文。历史
    // 会话仍在目录里，需要时用 `/session` 选择器打开即可。
    let session_catalog = session::FileSessionCatalog::new(&working_dir);
    session_catalog.adopt_legacy()?;
    let (entry, session) = session_catalog.create()?;
    let current_session = entry.name;
    let session_catalog: Arc<dyn session::SessionCatalog> = Arc::new(session_catalog);

    let agent = Agent::builder()
        .model_config(model_config)
        // 提示词以函数形式传入：每次解析都读取当前工作目录与项目 Agent.md，
        // 这样压缩重建后重新置顶的系统提示词始终反映最新的项目状态。
        .system_prompt(prompt::build(working_dir.clone()))
        .working_dir(working_dir)
        // 摘要的取舍口径：coding agent 关心文件 / 命令 / 报错 / 测试结果。
        // 结构与"不得推演、不得编下一步"等硬规则由 SDK 的 `COMPACTION_TEMPLATE` 追加，
        // 这里只写领域相关的偏好（见 `docs/compaction.md` 5.3）。
        .compression_instruction(
            r"
            你在为 coding agent 压缩对话上下文。请忠实保留用户下达的原始指令与约束，
            以及继续任务所必需的信息：关键决策及其原因、当前进度与状态、未解决的问题、
            涉及的文件路径、执行过的命令、遇到的错误和测试结果。删除重复、闲聊与过时的中间想法。
        ",
        )
        .tools(tool_manager)
        .tool_context(tool_context)
        .session(session)
        .build()?;

    interface::run(
        agent,
        catalog,
        session_catalog,
        Some(current_session),
        needs_login,
    )
    .await
    .map_err(|error| AgentError::Other(Box::new(error)))?;

    Ok(())
}
