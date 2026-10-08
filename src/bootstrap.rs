//! 应用装配（Bootstrap）：把「配置从哪来」与「用哪个界面渲染」解耦。
//!
//! 在引入桌面界面（`docs/desktop-interface.md`）之前，装配逻辑写死在 `main.rs`
//! 里、装配完直接调 `interface::run`。现在把它抽成一个 [`Bootstrap`]：**TUI 与
//! 桌面界面共享同一份装配产物**（同一个 `Agent` + catalogs），只是渲染方式不同。
//!
//! 这是「兄弟界面、共享 LCA」里的 **LCA 之一**：SDK 的 `Agent` 是另一个。
//! 两个界面都是它的消费者，谁都不独占装配。

use std::path::PathBuf;
use std::sync::Arc;

use shirley_agent_sdk::{Agent, AgentError, ModelConfig, ToolManager};

use crate::models::{self, ModelCatalog};
use crate::prompt;
use crate::session::{self, SessionCatalog};
use crate::settings::Settings;
use crate::tools;

/// 装配完成的运行时依赖：两个界面都能拿它起界面。
pub struct Bootstrap {
    /// 已装配好的 Agent（模型配置 / 提示词 / 工具 / 会话均已注入）。
    pub agent: Agent,
    /// 模型目录（`/model` 用），异步拉取、失败回退静态列表。
    pub model_catalog: Arc<dyn ModelCatalog>,
    /// 会话目录（`/session` 用）。
    pub session_catalog: Arc<dyn SessionCatalog>,
    /// 启动时新建的会话名（供页脚显示 / 初始绑定）。
    pub current_session: Option<String>,
    /// 缺模型服务配置（缺 `base_url`）：界面据此进入 `/login` 引导。
    pub needs_login: bool,
}

impl Bootstrap {
    /// 装配全部运行时依赖。工作目录由 [`crate::prompt::workspace_root`] 决定。
    ///
    /// 这里的顺序与原 `main.rs` 完全一致，抽出来只是为了给两个界面共用。
    pub fn assemble(working_dir: PathBuf) -> Result<Self, AgentError> {
        // 配置装载（方案 A）：优先级 `内置默认 < 全局 config.toml < 工作区
        // .shirley/config.toml < 环境变量`。
        let settings = Settings::load_default(&working_dir)
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        let needs_login = !settings.is_configured();

        // 模型目录：默认从 chat completions 的 base_url 推导 `/v1/models`，
        // 也可用配置里的 `models_url` 覆盖。拉取失败回退内置静态列表。
        let models_url = settings
            .models_url
            .clone()
            .unwrap_or_else(|| models::models_endpoint(&settings.base_url));
        let model_catalog: Arc<dyn ModelCatalog> = Arc::new(models::RemoteCatalog::new(
            models_url,
            settings.api_key.clone(),
            models::StaticCatalog::builtin().entries(),
        ));

        // 工具：bash / read_file 总是注册；web_search 未配置时不入表。
        let mut tool_manager = ToolManager::new();
        let _ = tool_manager.register(tools::bash_tool::tool());
        let _ = tool_manager.register(tools::read_file_tool::tool());
        if let Err(error) = tool_manager.register(tools::web_search_tool::tool()) {
            eprintln!("[web_search] 未启用：{error}");
        }

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

        // 会话持久化：多会话布局 `<root>/.shirley/sessions/<name>.jsonl`。
        // 启动直接开一份新会话（而非恢复最近修改的旧会话）。
        let file_catalog = session::FileSessionCatalog::new(&working_dir);
        file_catalog
            .adopt_legacy()
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        let (entry, session) =
            file_catalog
                .create()
                .map_err(|error| AgentError::Other(Box::new(error)))?;
        let current_session = entry.name;
        let session_catalog: Arc<dyn SessionCatalog> = Arc::new(file_catalog);

        let agent = Agent::builder()
            .model_config(model_config)
            // 提示词以函数形式传入：每次解析都读取当前工作目录与项目 Agent.md。
            .system_prompt(prompt::build(working_dir.clone()))
            .working_dir(working_dir)
            .compression_instruction(
                r"
            你在为 coding agent 压缩对话上下文。请忠实保留用户下达的原始指令与约束，
            以及继续任务所必需的信息：关键决策及其原因、当前进度与状态、未解决的问题、
            涉及的文件路径、执行过的命令、遇到的错误和测试结果。删除重复、闲聊与过时的中间想法。
        ",
            )
            .tools(tool_manager)
            .session(session)
            .build()?;

        Ok(Self {
            agent,
            model_catalog,
            session_catalog,
            current_session: Some(current_session),
            needs_login,
        })
    }
}
