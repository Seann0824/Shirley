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

use shirley_agent_sdk::{
    Agent, AgentError, CompressionConfig, Message, ModelConfig, SystemPrompt, ToolManager,
};

use crate::models::{self, ModelCatalog};
use crate::prompt;
use crate::session::{self, SessionCatalog};
use crate::settings::Settings;
use crate::todo::{TodoContextProvider, TodoStore, TodoTool};
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
    /// 压缩策略（触发阈值 / 保留比例 / 摘要模板）。来自配置，所有会话共用。
    compression_config: CompressionConfig,
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

        // 压缩策略：配置里没写就沿用 SDK 默认（`CompressionConfig::default`）。
        let compression_config = settings.compression.to_sdk();

        // 会话持久化：多会话布局 `<root>/.shirley/sessions/<name>.jsonl`。
        let file_catalog = session::FileSessionCatalog::new(&working_dir);
        file_catalog
            .adopt_legacy()
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        let session_catalog: Arc<dyn SessionCatalog> = Arc::new(file_catalog);

        Ok(Self {
            model_config,
            system_prompt: prompt::build(working_dir.clone()),
            compression_config,
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
    /// 模型配置 / 系统提示词 / 压缩策略由工厂复用（`clone`）；`ToolManager` 与任务
    /// 账本按实例新建——因此不同会话的 `Agent` 互不共享可变状态，可真正并行。
    pub fn build_agent(&self, messages: Vec<Message>) -> Result<Agent, AgentError> {
        // 任务账本（应用层能力）：每个 `Agent` 一份 `TodoStore`，同时交给
        // `todo` 工具（写）与 `TodoContextProvider`（每轮末尾注入，跨压缩存活）。
        // 两者共享同一 `Arc`，账本状态随会话隔离。
        let todo_store = Arc::new(TodoStore::new());

        // `ToolManager` 非 `Clone`，每次造 `Agent` 都新建一份并重新注册工具：
        // 工具定义稳定（prefix 缓存友好），`on_register` 钩子（如 web_search 的
        // 凭据注入）各自执行一次——状态本就按工具实例隔离，可接受（决策 5）。
        let mut tool_manager = assemble_tools();
        let _ = tool_manager.register(TodoTool::new(todo_store.clone()));

        Agent::builder()
            .model_config(self.model_config.clone())
            .system_prompt(self.system_prompt.clone())
            .working_dir(self.working_dir.clone())
            .compression_instruction(COMPRESSION_INSTRUCTION)
            .compression_config(self.compression_config.clone())
            .context_provider(Arc::new(TodoContextProvider::new(todo_store)) as Arc<dyn shirley_agent_sdk::ContextProvider>)
            .tools(tool_manager)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::session::SessionManager;
    use crate::models::StaticCatalog;
    use crate::session::FileSessionCatalog;
    use shirley_agent_sdk::ModelProtocol;

    fn test_agent() -> Agent {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        Agent::builder().model_config(config).build().unwrap()
    }

    fn factory(root: PathBuf) -> AgentFactory {
        AgentFactory {
            model_config: ModelConfig::builder()
                .protocol(ModelProtocol::ChatCompletions)
                .base_url("http://localhost")
                .model("test")
                .build(),
            system_prompt: SystemPrompt::from("test"),
            compression_config: CompressionConfig::default(),
            working_dir: root.clone(),
            model_catalog: Arc::new(StaticCatalog::builtin()) as Arc<dyn ModelCatalog>,
            session_catalog: Arc::new(FileSessionCatalog::new(&root)) as Arc<dyn SessionCatalog>,
            needs_login: false,
        }
    }

    fn tmp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shirley_bootstrap_{tag}_{}", std::process::id()))
    }

    /// 回归：删除**当前**会话后，前台指针必须落到一份新的空会话，而不是悬在被删会话上。
    ///
    /// 此前 `agent_delete_session` 只删磁盘、不动 `SessionManager`，`active` 仍指向被删
    /// 会话，`agent_current_session` 继续返回旧名，前端又把它打开——界面因此卡在被删会话。
    #[test]
    fn deleting_active_session_switches_to_fresh_empty_session() {
        let root = tmp_root("del_active");
        let catalog: Arc<dyn SessionCatalog> = Arc::new(FileSessionCatalog::new(&root));
        let mut manager = SessionManager::with_factory(
            test_agent(),
            Some("s".into()),
            catalog,
            Arc::new(factory(root.clone())),
            None,
        );
        assert_eq!(manager.active_name(), Some("s"));

        manager.delete_session("s").unwrap();

        let active = manager.active_name().map(str::to_owned);
        assert_ne!(active.as_deref(), Some("s"), "前台不应再停在被删会话上");
        assert!(active.is_some(), "删掉前台会话后应补一份空会话");
        assert!(manager.active_agent().is_some(), "新空会话应已就绪");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 运行中的会话不得被删除（避免删掉正在写入的日志）。
    #[test]
    fn deleting_running_session_is_rejected() {
        let root = tmp_root("del_running");
        let catalog: Arc<dyn SessionCatalog> = Arc::new(FileSessionCatalog::new(&root));
        let mut manager = SessionManager::with_factory(
            test_agent(),
            Some("s".into()),
            catalog,
            Arc::new(factory(root.clone())),
            None,
        );
        assert!(manager.begin_turn("s", "hello".into()).is_some());

        assert!(manager.delete_session("s").is_err(), "运行中会话应拒绝删除");
        assert_eq!(manager.active_name(), Some("s"), "拒绝后前台不应改变");

        let _ = std::fs::remove_dir_all(&root);
    }
}
