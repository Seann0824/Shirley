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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use shirley_agent_sdk::{
    Agent, AgentError, CompressionConfig, ContextProvider, Message, ModelConfig, SystemPrompt,
    ToolManager,
};

use crate::memory::{
    self, CurateOutcome, CuratorError, Embedder, EmbeddingConfig, MemoryContextProvider,
    MemoryRuntime, MemoryStore,
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
    /// 记忆根目录：`[全局, 工作区]`（`docs/memory.md` §3.1）。`roots[0]`（全局）是
    /// **写入主根**，读时合并（工作区覆盖全局）。所有会话共用同一份目录。
    memory_roots: Vec<PathBuf>,
    /// curator 独立模型配置（V2-C，`docs/memory.md` §10）：`[memory]` 表配了
    /// `curator_*` 时用它，否则为 `None`——curator 回退主模型（诚实回退，不假装异源）。
    curator_model_config: Option<ModelConfig>,
    /// 条目数达到该阈值时，在 curation 后触发一次定期整理（睡眠学习）。`0` = 不自动触发。
    consolidate_after_entries: usize,
    /// **V2.5** embedding 客户端：`[memory]` 配齐 `embedding_base_url` +
    /// `embedding_model` 时才构造（`None` = 纯 BM25，诚实降级）。所有会话共用。
    embedder: Option<Arc<Embedder>>,
}

/// 全局记忆相对 `config_dir` 的位置（跨项目）。
const GLOBAL_MEMORY_REL: &str = "shirley/memory";
/// 工作区记忆相对工作区根的位置（随仓库走）。
const WORKSPACE_MEMORY_REL: &str = ".shirley/memory";

/// 记忆根目录：`[全局, 工作区]`。拿不到 `config_dir` 时退化为仅工作区。
fn memory_roots(working_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(config_dir) = dirs::config_dir() {
        roots.push(config_dir.join(GLOBAL_MEMORY_REL));
    }
    roots.push(working_dir.join(WORKSPACE_MEMORY_REL));
    roots
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

        // curator 独立模型（V2-C）：`[memory]` 配了 curator_* 才另起一份配置，
        // 否则为 `None`（回退主模型）。缺省继承主配置的流式 / thinking 等字段。
        let curator_model_config = build_curator_config(&settings, &model_config);

        // 记忆混合检索的 embedding 客户端（V2.5）：配齐端点 + 模型才启用语义腿；
        // 否则为 `None`（检索退化为纯 BM25，绝不假装有语义腿）。
        let embedder = build_embedder(&settings);

        Ok(Self {
            model_config,
            system_prompt: prompt::build(working_dir.clone()),
            compression_config,
            working_dir: working_dir.clone(),
            model_catalog,
            session_catalog,
            needs_login,
            memory_roots: memory_roots(&working_dir),
            curator_model_config,
            consolidate_after_entries: settings.memory.consolidate_after_entries.unwrap_or(0),
            embedder,
        })
    }

    /// 按**已恢复的消息工作集**造一个独立 `Agent`（`docs/multi-session.md` 决策 5）。
    ///
    /// 会话持久化已上移应用层：调用方先 `SessionStore::load()` 拿回历史，再交给这里
    /// 起 `Agent`（空 `Vec` = 全新会话）。`Agent::new` 会把 system 现生成置顶——
    /// 日志里不含 system，故恢复出的工作集必然与冷启动一致。
    ///
    /// 模型配置 / 系统提示词 / 压缩策略由工厂复用（`clone`）；`ToolManager`、任务
    /// 账本、记忆运行时按实例新建——因此不同会话的 `Agent` 互不共享可变状态，可真正
    /// 并行。
    ///
    /// 返回 [`BuiltAgent`]：`Agent` 之外**额外带回记忆运行时句柄**——因为
    /// `ContextProvider` 是 trait object，驱动方（TUI / desktop）拿不到具体类型，
    /// 需要在每轮前经该句柄 `set_query`（`docs/memory.md` §4.2）。
    pub fn build_agent(&self, messages: Vec<Message>) -> Result<BuiltAgent, AgentError> {
        // 任务账本（应用层能力）：每个 `Agent` 一份 `TodoStore`，同时交给
        // `todo` 工具（写）与 `TodoContextProvider`（每轮末尾注入，跨压缩存活）。
        // 两者共享同一 `Arc`，账本状态随会话隔离。
        let todo_store = Arc::new(TodoStore::new());

        // 记忆（应用层能力）：每个 `Agent` 一份 `MemoryRuntime`（独立 query 槽，
        // 多会话并发互不干扰），共享同一份 memory 目录（全局 + 工作区）。
        let memory = Arc::new(MemoryRuntime::with_embedder(
            self.memory_store(),
            self.embedder.clone(),
        ));

        // `ToolManager` 非 `Clone`，每次造 `Agent` 都新建一份并重新注册工具：
        // 工具定义稳定（prefix 缓存友好），`on_register` 钩子（如 web_search 的
        // 凭据注入）各自执行一次——状态本就按工具实例隔离，可接受（决策 5）。
        let mut tool_manager = assemble_tools();
        let _ = tool_manager.register(TodoTool::new(todo_store.clone()));

        // SDK 的 `Agent` 只接受**单个** `context_provider`，而这里要注入两份
        // （任务账本 + 记忆）——用 `CompositeContextProvider` 顺序拼接。
        let provider = CompositeContextProvider::new(vec![
            Arc::new(TodoContextProvider::new(todo_store)) as Arc<dyn ContextProvider>,
            Arc::new(MemoryContextProvider::new(memory.clone())) as Arc<dyn ContextProvider>,
        ]);

        let agent = Agent::builder()
            .model_config(self.model_config.clone())
            .system_prompt(self.system_prompt.clone())
            .working_dir(self.working_dir.clone())
            .compression_instruction(COMPRESSION_INSTRUCTION)
            .compression_config(self.compression_config.clone())
            .context_provider(Arc::new(provider) as Arc<dyn ContextProvider>)
            .tools(tool_manager)
            .messages(messages)
            .build()?;
        Ok(BuiltAgent { agent, memory })
    }

    /// 本工厂的记忆存储（全局 + 工作区多根，读合并、写落主根）。
    pub fn memory_store(&self) -> MemoryStore {
        MemoryStore::with_roots(self.memory_roots.clone())
    }

    /// 会话结束时触发一次增量 curation（`docs/memory.md` §5.1）。
    ///
    /// 用**最小 `Agent`**（仅借模型配置，不带工具 / 系统提示词 / 记忆注入）调
    /// [`Agent::complete`]——一次性、非流式、不碰任何会话工作集。抽取出的候选经
    /// 确定性自检后合入，并重建 `index.md`。
    ///
    /// 只读环境探测（§5.4）V1 不做，诚实降级为纯轨迹 curation。
    pub async fn curate(
        &self,
        messages: &[Message],
        session_ref: &str,
    ) -> Result<CurateOutcome, CuratorError> {
        let agent = self.curator_agent()?;
        let store = self.memory_store();
        let sessions_dir = self.sessions_dir();
        memory::curate(
            &agent,
            &store,
            messages,
            session_ref,
            Some(sessions_dir.as_path()),
        )
        .await
    }

    /// 定期整理（睡眠学习，`docs/memory.md` §5.2）：全量扫描 → 模型提议 → 自检 → 合入。
    ///
    /// 与 [`AgentFactory::curate`] 共用 curator 模型（`[memory]` 配了就用独立模型）。
    /// `sessions_dir` 存在时做**回原始证据核查**。
    pub async fn consolidate(&self) -> Result<memory::ConsolidateOutcome, CuratorError> {
        let agent = self.curator_agent()?;
        let store = self.memory_store();
        let sessions_dir = self.sessions_dir();
        let sessions_dir = sessions_dir.exists().then_some(sessions_dir.as_path());
        memory::consolidate(&agent, &store, sessions_dir).await
    }

    /// 当前记忆条目数是否达到定期整理阈值（`[memory] consolidate_after_entries`）。
    ///
    /// 阈值 `0` 表示不自动触发。读目录失败按"不触发"处理（best-effort）。
    pub fn should_consolidate(&self) -> bool {
        if self.consolidate_after_entries == 0 {
            return false;
        }
        self.memory_store()
            .list_entries()
            .map(|entries| entries.len() >= self.consolidate_after_entries)
            .unwrap_or(false)
    }

    /// 造 curator 用的**最小 `Agent`**：仅借模型配置，不带工具 / 系统提示词 / 记忆注入。
    fn curator_agent(&self) -> Result<Agent, CuratorError> {
        let config = self
            .curator_model_config
            .clone()
            .unwrap_or_else(|| self.model_config.clone());
        Agent::builder()
            .model_config(config)
            .build()
            .map_err(|error| CuratorError::Model(error.to_string()))
    }

    /// 会话日志目录（curator 回证据核查用）。
    fn sessions_dir(&self) -> PathBuf {
        self.working_dir.join(".shirley").join("sessions")
    }
}

/// 从 `[memory]` 配置构造 curator 独立模型配置；未配置 curator 端点/模型时返回 `None`
/// （curator 回退主模型）。
///
/// 继承主配置的 `stream` / `thinking` / `reasoning_effort` / `context_window_tokens`
/// 等字段，只覆盖 curator 显式给出的端点 / 密钥 / 模型 / 协议——避免"配了 curator 模型
/// 却丢了主配置的其它字段"。
fn build_curator_config(settings: &Settings, main: &ModelConfig) -> Option<ModelConfig> {
    let mem = &settings.memory;
    // 没配任何 curator 键就不另起配置。
    if mem.curator_model.is_none()
        && mem.curator_base_url.is_none()
        && mem.curator_api_key.is_none()
        && mem.curator_protocol.is_none()
    {
        return None;
    }
    let mut config = main.clone();
    if let Some(model) = mem.curator_model.as_ref().filter(|m| !m.trim().is_empty()) {
        config.model = model.clone();
    }
    if let Some(base_url) = mem.curator_base_url.as_ref().filter(|u| !u.trim().is_empty()) {
        config.base_url = base_url.clone();
    }
    if let Some(api_key) = mem.curator_api_key.clone() {
        config.api_key = (!api_key.is_empty()).then_some(api_key);
    }
    if let Some(protocol) = mem.curator_protocol.as_deref() {
        if let Ok(parsed) = crate::settings::parse_protocol_name(protocol) {
            config.protocol = parsed;
        }
    }
    Some(config)
}

/// 从 `[memory]` 配置构造 embedding 客户端（V2.5 混合检索的语义腿）。
///
/// 需 `embedding_base_url` + `embedding_model` 同时给出才启用；密钥可缺省（本地服务
/// 常无鉴权）。任缺其一 → `None`（检索退化为纯 BM25）。
fn build_embedder(settings: &Settings) -> Option<Arc<Embedder>> {
    let mem = &settings.memory;
    let endpoint = mem
        .embedding_base_url
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    let model = mem
        .embedding_model
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    let api_key = mem
        .embedding_api_key
        .clone()
        .filter(|key| !key.is_empty());
    let config = EmbeddingConfig {
        endpoint: endpoint.to_string(),
        api_key,
        model: model.to_string(),
    };
    Embedder::new(config).ok().map(Arc::new)
}

/// [`AgentFactory::build_agent`] 的产物：`Agent` 及其配套的记忆运行时句柄。
pub struct BuiltAgent {
    pub agent: Agent,
    /// 记忆运行时（每会话一份）：驱动方在发起一轮前 `set_query`，会话结束交给 curator。
    pub memory: Arc<MemoryRuntime>,
}

/// 把多个 [`ContextProvider`] 拼成一个：按顺序拼接各自 `context()`，全空返回 `None`。
///
/// SDK �� `Agent` 只接受单个 `context_provider`，而应用层要同时注入任务账本与记忆
/// ——这是纯应用层的胶水，不进 SDK。
pub struct CompositeContextProvider {
    providers: Vec<Arc<dyn ContextProvider>>,
}

impl CompositeContextProvider {
    pub fn new(providers: Vec<Arc<dyn ContextProvider>>) -> Self {
        Self { providers }
    }
}

impl ContextProvider for CompositeContextProvider {
    fn context(&self) -> Option<String> {
        let parts: Vec<String> = self.providers.iter().filter_map(|p| p.context()).collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
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
            memory_roots: vec![root.join(".shirley/memory")],
            curator_model_config: None,
            consolidate_after_entries: 0,
            embedder: None,
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

    /// 空实现 provider，便于验证 CompositeContextProvider 的拼接 / 短路语义。
    struct StaticProvider(Option<&'static str>);

    impl ContextProvider for StaticProvider {
        fn context(&self) -> Option<String> {
            self.0.map(str::to_owned)
        }
    }

    /// 回归：CompositeContextProvider 顺序拼接多个 provider 的 `context()`；
    /// 全空（或空列表）时返回 `None`，不注入空内容。
    #[test]
    fn composite_context_provider_concatenates_and_short_circuits() {
        let both = CompositeContextProvider::new(vec![
            Arc::new(StaticProvider(Some("账本"))) as Arc<dyn ContextProvider>,
            Arc::new(StaticProvider(Some("记忆"))) as Arc<dyn ContextProvider>,
        ]);
        assert_eq!(both.context().as_deref(), Some("账本\n\n记忆"));

        // 一方为空：只保留有内容的一侧（不能拼出多余分隔符）。
        let one = CompositeContextProvider::new(vec![
            Arc::new(StaticProvider(None)) as Arc<dyn ContextProvider>,
            Arc::new(StaticProvider(Some("记忆"))) as Arc<dyn ContextProvider>,
        ]);
        assert_eq!(one.context().as_deref(), Some("记忆"));

        // 全空 / 空列表：返回 `None`，绝不注入空串。
        let none = CompositeContextProvider::new(vec![
            Arc::new(StaticProvider(None)) as Arc<dyn ContextProvider>,
        ]);
        assert_eq!(none.context(), None);
        assert_eq!(CompositeContextProvider::new(vec![]).context(), None);
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
            None,
        );
        assert!(manager.begin_turn("s", "hello".into()).is_some());

        assert!(manager.delete_session("s").is_err(), "运行中会话应拒绝删除");
        assert_eq!(manager.active_name(), Some("s"), "拒绝后前台不应改变");

        let _ = std::fs::remove_dir_all(&root);
    }
}
