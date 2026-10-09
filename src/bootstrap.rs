//! 应用装配（Bootstrap）：把「配置从哪来」与「用哪个界面渲染」解耦。
//!
//! 在引入桌面界面（`docs/desktop-interface.md`）之前，装配逻辑写死在 `main.rs`
//! 里、装配完直接调 `interface::run`。现在把它抽成一个 [`AgentFactory`]：
//! **TUI 与桌面界面共享同一份装配产物**（同一套模型配置 / 工具 / 会话目录），
//! 只是渲染方式不同。
//!
//! 这是「兄弟界面、共享 LCA」里的 **LCA 之一**：SDK 的 `Agent` 是另一个。
//! 两个界面都是它的消费者，谁都不独占装配。
//!
//! 多会话（`docs/multi-session.md` 决策 5）：装配不再是「造一个 `Agent`」，
//! 而是产出 [`AgentFactory`]——**一个能按会话反复造 `Agent` 的工厂**。
//! 每个会话各自 [`AgentFactory::build_agent`] 出一个独立 `Agent`（模型配置 /
//! 工具定义由工厂复用，`todo` / 会话日志在 `Agent::new` 内部按实例
//! 隔离），从而支持多会话并行。

use std::path::PathBuf;
use std::sync::Arc;

use shirley_agent_sdk::{Agent, AgentError, Message, ModelConfig, SystemPrompt, ToolManager};

use crate::models::{self, ModelCatalog};
use crate::prompt;
use crate::session::{self, SessionCatalog};
use crate::settings::Settings;
use crate::tools;

/// 压缩指令：所有会话共用（不随会话变化）。
const COMPRESSION_INSTRUCTION: &str = r"
            你在为 coding agent 压缩对话上下文。请忠实保留用户下达的原始指令与约束，
            以及继续任务所必需的信息：关键决策及其原因、当前进度与状态、未解决的问题、
            涉及的文件路径、执行过的命令、遇到的错误和测试结果。删除重复、闲聊与过时的中间想法。
        ";

/// 装配完成的运行时依赖：两个界面都能拿它起界面。
///
/// 它本身**不持有 `Agent`**——多会话下每个会话各有一个 `Agent`，由
/// [`AgentFactory::build_agent`] 按需造出。工厂持有的是「造 `Agent` 所需的一切」。
pub struct AgentFactory {
    /// 模型配置：所有会话共用（`Clone` 后交给每个 `Agent`）。
    model_config: ModelConfig,
    /// 系统提示词（函数形式：每次解析读当前工作目录与项目 `Agent.md`）。
    system_prompt: SystemPrompt,
    /// 工作目录（工作区根）。桌面界面用它做 `@` 文件检索（`workspace_search`）；
    /// TUI-only 构建下没人读，故放行 dead_code。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub working_dir: PathBuf,
    /// 模型目录（`/model` 用），异步拉取、失败回退静态列表。
    pub model_catalog: Arc<dyn ModelCatalog>,
    /// 会话目录（`/session` 用）。
    pub session_catalog: Arc<dyn SessionCatalog>,
    /// 缺模型服务配置（缺 `base_url`）：界面据此进入 `/login` 引导。
    pub needs_login: bool,
}

impl AgentFactory {
    /// 装配全部运行时依赖。工作目录由 [`crate::prompt::workspace_root`] 决定。
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
        let file_catalog = session::FileSessionCatalog::new(&working_dir);
        file_catalog
            .adopt_legacy()
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        let session_catalog: Arc<dyn SessionCatalog> = Arc::new(file_catalog);

        Ok(Self {
            model_config,
            system_prompt: prompt::build(working_dir.clone()),
            working_dir: working_dir.clone(),
            model_catalog,
            session_catalog,
            needs_login,
        })
    }

    /// 按**已恢复的消息工作集**造一个独立 `Agent`（`docs/multi-session.md` 决策 5）。
    ///
    /// 会话持久化已上移应用层：调用方先 `SessionStore::load()` 拿回历史，再交给这里
    /// 起 `Agent`（空 `Vec` = 全新会话）。`Agent::new` 会把 system 现生成置顶——
    /// 日志里不含 system，故恢复出的工作集必然与冷启动一致。
    ///
    /// 模型配置 / 系统提示词 / 工具定义由工厂复用（`clone`），`todo` 在
    /// `Agent::new` 内部按实例隔离——因此不同会话的 `Agent` 互不共享可变状态，可真正并行。
    pub fn build_agent(&self, messages: Vec<Message>) -> Result<Agent, AgentError> {
        // `ToolManager` 非 `Clone`，每次造 `Agent` 都新建一份并重新注册工具：
        // 工具定义稳定（prefix 缓存友好），`on_register` 钩子（如 web_search 的
        // 凭据注入）各自执行一次——状态本就按工具实例隔离，可接受（决策 5）。
        Agent::builder()
            .model_config(self.model_config.clone())
            .system_prompt(self.system_prompt.clone())
            .working_dir(self.working_dir.clone())
            .compression_instruction(COMPRESSION_INSTRUCTION)
            .tools(assemble_tools())
            .messages(messages)
            .build()
    }

}

/// 造一份注册好全部工具的 `ToolManager`（bash / read_file 总是注册；
/// web_search 未配置时不入表）。每个 `Agent` 各持一份，工具状态按实例隔离。
fn assemble_tools() -> ToolManager {
    let mut tool_manager = ToolManager::new();
    let _ = tool_manager.register(tools::bash_tool::tool());
    let _ = tool_manager.register(tools::read_file_tool::tool());
    if let Err(error) = tool_manager.register(tools::web_search_tool::tool()) {
        eprintln!("[web_search] 未启用：{error}");
    }
    tool_manager
}
