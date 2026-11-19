//! Application assembly (Bootstrap): decouples "where config comes from" from "which interface renders it".
//!
//! Before the desktop interface was introduced, the assembly logic was hardcoded in `main.rs`
//! and called `interface::run` directly after assembling. Now it is extracted into an [`AgentFactory`]:
//! **the TUI and the desktop interface share the same assembly output** (the same model config / tools / session catalog),
//! only the rendering differs.
//!
//! This is **one of the LCAs** in "sibling interfaces, shared LCA": the SDK's `Agent` is the other.
//! Both interfaces are its consumers; neither monopolizes assembly.
//!
//! Multi-session: assembly is no longer "build one `Agent`",
//! but produces an [`AgentFactory`] -- **a factory that can repeatedly build `Agent`s per session**.
//! Each session gets its own independent `Agent` via [`AgentFactory::build_agent`] (model config /
//! tool definitions are reused by the factory; `todo` / session logs are isolated per instance inside `Agent::new`),
//! thereby supporting parallel sessions.

use std::path::PathBuf;
use std::sync::Arc;

use shirley_agent_sdk::{
    Agent, AgentError, CompactionConfig, ContextProvider, Message, ModelConfig, SystemPrompt,
    ToolCallingMode, ToolManager,
};

use crate::artifact::ArtifactStore;
use crate::memory::{
    self, CurateOutcome, CuratorError, Embedder, EmbeddingConfig, MemoryContextProvider,
    MemoryRuntime, MemoryStore,
};
use crate::models::{self, ModelCatalog, ModelEntry};
use crate::prompt;
use crate::session::{self, SessionCatalog};
use crate::settings::Settings;
use crate::skills::{self, SkillCatalog, SkillContextProvider, SkillRuntime};
use crate::subagent::{
    DEFAULT_MAX_CONCURRENT, DEFAULT_MAX_DEPTH, DEFAULT_PER_TASK_MAX_STEPS, DEFAULT_WAIT_TIMEOUT_MS,
    SubagentLimits, SubagentManager, SubagentTool, ToolSelection,
};
use crate::todo::{TodoContextProvider, TodoStore};
use crate::tools;

/// Compression instruction: shared by all sessions (does not vary by session). The text lives in `prompts/compaction_instruction.md`.
const COMPRESSION_INSTRUCTION: &str = include_str!("../prompts/compaction_instruction.md");

/// The assembled runtime dependencies: both interfaces can use it to bring up their UI.
///
/// It **does not itself hold an `Agent`** -- under multi-session each session has its own `Agent`, built on demand by
/// [`AgentFactory::build_agent`]. What the factory holds is "everything needed to build an `Agent`".
pub struct AgentFactory {
    /// Model config: shared by all sessions (`Clone`d and handed to each `Agent`).
    model_config: ModelConfig,
    /// System prompt (function form: each resolution reads the current working dir and the project's `Agent.md`).
    system_prompt: SystemPrompt,
    /// Compression strategy (trigger threshold / retain ratio / summary template). From config, shared by all sessions.
    compaction_config: CompactionConfig,
    max_steps: usize,
    /// Model catalog (for `/model`), fetched asynchronously, falling back to a static list on failure.
    pub model_catalog: Arc<dyn ModelCatalog>,
    /// Session catalog (for `/session`).
    pub session_catalog: Arc<dyn SessionCatalog>,
    /// Missing model service config (missing `base_url`): the interface uses this to enter the `/login` guide.
    pub needs_login: bool,
    /// Memory root dirs: `[global, workspace]` (`docs/memory.md` §3.1). `roots[0]` (global) is the
    /// **primary write root**; reads merge (workspace overrides global). All sessions share the same dirs.
    memory_roots: Vec<PathBuf>,
    /// The curator's separate model config (V2-C, `docs/memory.md` §10): used when the `[memory]` table configures
    /// `curator_*`, otherwise `None` -- the curator falls back to the main model (honest fallback, does not pretend to be cross-source).
    curator_settings: crate::settings::MemorySettings,
    /// When the entry count reaches this threshold, trigger a periodic consolidation after curation (sleep learning). `0` = do not auto-trigger.
    consolidate_after_entries: usize,
    /// **V2.5** embedding client: constructed only when `[memory]` configures both `embedding_base_url` +
    /// `embedding_model` (`None` = pure BM25, honest degradation). Shared by all sessions.
    embedder: Option<Arc<Embedder>>,
    /// Artifact jail (`docs/render-parts.md`): where bytes a chat page may open live. One store
    /// per project -- the id a `RenderPart` carries is only resolvable against its own project.
    pub artifacts: Arc<ArtifactStore>,
    /// Skill catalog (`docs/skills.md` P0): scanned once at assembly, shared by all sessions (read-only, unchanging).
    /// Each `Agent` injects its Tier1 catalog at the end of each round via `SkillContextProvider`.
    skill_catalog: SkillCatalog,
    /// Subagent config (`docs/subagent.md` decision 8): resolved once at assembly, shared by all sessions.
    /// `enabled == false` means the `subagent` tool is not registered at all.
    subagent_enabled: bool,
    /// The three caps for child agents.
    subagent_limits: SubagentLimits,
    /// The default child tool set when the model omits `tools` (`None` = all tools).
    subagent_default_tools: Option<Vec<String>>,
    /// Default `wait` / blocking-dispatch timeout (ms).
    subagent_default_timeout_ms: u64,
    /// Bot specification: controls which capability tools are available to this factory's sessions.
    pub bot_spec: crate::bot::BotSpec,
    /// Where this bot's file tools work: the cwd of `bash`, the root `read` / `grep` resolve
    /// relative paths against, and the directory the system prompt names.
    workspace: PathBuf,
    /// The bot's handle on the task pool. Set by the runtime manager; `None` in tests and for
    /// the platform factory itself.
    tasks: Option<crate::pool::desk::TaskDesk>,
}

impl AgentFactory {
    /// Assemble all runtime dependencies.
    pub fn assemble() -> Result<Self, AgentError> {
        Self::assemble_at(crate::storage::global_root())
    }

    /// Explicit application-data root for isolated tests and assembly.
    pub(crate) fn assemble_at(global_root: PathBuf) -> Result<Self, AgentError> {
        let paths = crate::storage::StoragePaths::for_global(global_root)
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        let settings = Settings::load(Some(&paths.global.join("config.toml")), &|key| {
            std::env::var(key).ok()
        })
        .map_err(|error| AgentError::Other(Box::new(error)))?;
        let needs_login = !settings.is_configured()
            && crate::model_providers::default_config(&paths.global)
                .map_err(|error| AgentError::Other(Box::new(std::io::Error::other(error))))?
                .is_none();

        // Model catalog: by default derives `/v1/models` from the chat completions base_url,
        // but can be overridden with `models_url` in config. On fetch failure falls back to the built-in static list.
        let models_url = settings
            .models_url
            .clone()
            .unwrap_or_else(|| models::models_endpoint(&settings.base_url));
        let legacy_catalog: Arc<dyn ModelCatalog> = Arc::new(models::RemoteCatalog::new(
            models_url,
            settings.api_key.clone(),
            models::StaticCatalog::builtin().entries(),
        ));
        let model_catalog: Arc<dyn ModelCatalog> =
            Arc::new(crate::model_providers::ConfiguredCatalog {
                global: paths.global.clone(),
                fallback: legacy_catalog,
            });

        let mut model_config = ModelConfig::builder()
            .protocol(settings.protocol)
            .base_url(settings.base_url.clone())
            .maybe_api_key(settings.api_key.clone())
            .model(settings.model.clone())
            .stream(true)
            .thinking(settings.thinking)
            .reasoning_effort(settings.reasoning_effort.clone())
            .maybe_extra_body(settings.extra_body.clone())
            // Tool transport mode (`docs/tool-calling-refactor.md`): if config says so, do it;
            // otherwise default to `Native`. The "per model declaration" when switching models is handled by `model_config_for`.
            .tool_calling(resolve_tool_calling(&settings))
            .build();
        model_config.context_window_tokens = Some(settings.context_window_tokens);
        model_config.supports_images = settings.supports_images;

        // Compression strategy: if config does not specify it, follow the SDK default (`CompactionConfig::default`).
        let compaction_config = settings.compression.to_sdk();

        let session_catalog: Arc<dyn SessionCatalog> =
            Arc::new(session::FileSessionCatalog::new(&paths.state));

        // The curator's separate model (V2-C): only if `[memory]` configures curator_* do we build a separate config,
        // otherwise `None` (falls back to the main model). Defaults inherit the main config's stream / thinking fields.
        let curator_settings = settings.memory.clone();

        // The embedding client for memory hybrid retrieval (V2.5): the semantic leg is enabled only with both endpoint + model;
        // otherwise `None` (retrieval degrades to pure BM25, never pretends there is a semantic leg).
        let embedder = build_embedder(&settings);

        // Skill catalog (`docs/skills.md` P0): scans user-level skills once at assembly.
        // In bot-first mode there is no per-workspace project root, so only user-level roots are scanned.
        let skill_catalog = SkillCatalog::discover(&paths.global, skills::user_home().as_deref());

        // Subagent config (`docs/subagent.md` decision 8): resolve the caps once. `enabled` defaults to
        // true; a value of `0` for a cap means "unset" and falls back to the built-in default.
        let subagent = &settings.subagent;
        let subagent_enabled = subagent.enabled.unwrap_or(true);
        let subagent_limits = SubagentLimits {
            max_concurrent: subagent
                .max_concurrent
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_MAX_CONCURRENT),
            max_depth: subagent
                .max_depth
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_MAX_DEPTH),
            per_task_max_steps: subagent
                .per_task_max_steps
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_PER_TASK_MAX_STEPS),
        };
        let subagent_default_tools = subagent
            .default_tools
            .clone()
            .filter(|tools| !tools.is_empty());
        let subagent_default_timeout_ms = subagent
            .default_timeout_ms
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);

        let workspace = paths.global.join("workspace");
        Ok(Self {
            model_config,
            system_prompt: prompt::build(
                workspace.clone(),
                prompt::Role::Worker {
                    subagents: subagent_enabled,
                },
                None,
            ),
            compaction_config,
            max_steps: settings.max_steps,
            model_catalog,
            session_catalog,
            needs_login,
            artifacts: Arc::new(ArtifactStore::new(paths.artifacts())),
            memory_roots: paths.memory_roots(),
            curator_settings,
            consolidate_after_entries: settings.memory.consolidate_after_entries.unwrap_or(0),
            embedder,
            skill_catalog,
            subagent_enabled,
            subagent_limits,
            subagent_default_tools,
            subagent_default_timeout_ms,
            bot_spec: crate::bot::BotSpec::default_bot(),
            workspace,
            tasks: None,
        })
    }

    /// Build an independent `Agent` from the **restored message working set**.
    ///
    /// Session persistence has moved up to the application layer: the caller first `SessionStore::load()`s the history, then hands it here
    /// to start an `Agent` (empty `Vec` = a fresh session). `Agent::new` generates the system prompt fresh and puts it at the top --
    /// the log contains no system, so the restored working set is necessarily consistent with a cold start.
    ///
    /// Model config / system prompt / compression strategy are reused by the factory (`clone`); the `ToolManager`, task
    /// ledger, and memory runtime are created per instance -- so `Agent`s of different sessions share no mutable state and can truly
    /// run in parallel.
    ///
    /// Returns [`BuiltAgent`]: besides the `Agent`, **it brings back a memory runtime handle** -- because
    /// `ContextProvider` is a trait object, the driver (TUI / desktop) cannot get the concrete type and
    /// needs to `set_query` via this handle before each round (`docs/memory.md` §4.2).
    pub fn build_agent(self: &Arc<Self>, messages: Vec<Message>) -> Result<BuiltAgent, AgentError> {
        self.build_agent_with(messages, None, None)
    }

    fn system_prompt_for(&self, role: prompt::Role) -> SystemPrompt {
        prompt::build(
            self.workspace.clone(),
            role,
            self.bot_spec.system_prompt_extra.clone(),
        )
    }

    /// Build one `Agent`, optionally as a **child** with a filtered tool table (`docs/subagent.md` P0).
    ///
    /// - `tool_filter == None`: the **top level**. A [`SubagentManager`] is created (holding this
    ///   factory) and the `subagent` tool is registered, so this `Agent` can dispatch children.
    /// - `tool_filter == Some(selection)`: a **child**. No manager and no `subagent` tool are created
    ///   (the recursion guard: a child can never dispatch a child); the tool table is
    ///   `(allow ?? all) - deny - {subagent}`.
    ///
    /// `max_steps` overrides the factory default (children use their own `per_task_max_steps` budget).
    pub fn build_agent_with(
        self: &Arc<Self>,
        messages: Vec<Message>,
        tool_filter: Option<&ToolSelection>,
        max_steps: Option<usize>,
    ) -> Result<BuiltAgent, AgentError> {
        self.build(messages, tool_filter, max_steps, true)
    }

    /// The agent that runs a dispatched task: everything a conversation has except the task
    /// desk, so a worker cannot dispatch workers of its own.
    pub fn build_worker_agent(
        self: &Arc<Self>,
        messages: Vec<Message>,
    ) -> Result<BuiltAgent, AgentError> {
        self.build(messages, None, None, false)
    }

    /// Give this bot's conversations a handle on the task pool.
    #[must_use]
    pub fn with_tasks(mut self, desk: crate::pool::desk::TaskDesk) -> Self {
        self.tasks = Some(desk);
        self
    }

    fn build(
        self: &Arc<Self>,
        messages: Vec<Message>,
        tool_filter: Option<&ToolSelection>,
        max_steps: Option<usize>,
        with_desk: bool,
    ) -> Result<BuiltAgent, AgentError> {
        // Task ledger (application-layer capability): one `TodoStore` per `Agent`, given both to the
        // `todo` tool (write) and `TodoContextProvider` (injected at the end of each round, surviving compaction).
        // Both share the same `Arc`, so the ledger state is isolated per session.
        let todo_store = Arc::new(TodoStore::new());

        // Memory (application-layer capability): one `MemoryRuntime` per `Agent` (an independent query slot,
        // so concurrent sessions do not interfere), sharing the same memory dirs (global + workspace).
        let memory = Arc::new(MemoryRuntime::with_embedder(
            self.memory_store(),
            self.embedder.clone(),
        ));

        // `ToolManager` is not `Clone`; each `Agent` build creates a fresh one and re-registers tools:
        // tool definitions are stable (prefix-cache friendly), and `on_register` hooks (e.g. web_search's
        // credential injection) each run once -- state is isolated per tool instance anyway, which is acceptable (decision 5).
        let mut tool_manager = assemble_tools(self.artifacts.as_ref(), &self.bot_spec);
        // The bot's own directory is where its file tools work. Created on demand; a failure
        // surfaces later as a tool error naming the path.
        let _ = std::fs::create_dir_all(&self.workspace);
        tool_manager
            .context_mut()
            .insert(tools::read::WorkspaceRoots(vec![self.workspace.clone()]));

        // A bot's conversation with the user (as opposed to a worker or a subagent): it talks and
        // hands work off, so it gets the task desk and none of the worker machinery -- no `todo`
        // ledger (and its nagging reminder), no subagents, no computer use.
        let conversation = with_desk && tool_filter.is_none() && self.tasks.is_some();

        if !conversation {
            let _ = tool_manager.register(tools::TodoTool::new(Arc::clone(&todo_store)));
        }
        // Active memory recall tool (read-only): shares the same `MemoryRuntime` as automatic injection.
        // Memory writes go **through no** tool; they are done only by the background curator (`docs/memory.md` decision 3).
        let _ = tool_manager.register(tools::RecallTool::new(Arc::clone(&memory)));
        // Platform administration: any bot can define a new bot.
        let _ = tool_manager.register(tools::CreateBotTool::new(self.global_root().join("bots")));
        // Answering events from connected services, where any is connected.
        if tool_filter.is_none()
            && crate::connectors::load(self.global_root())
                .iter()
                .any(|connection| connection.kind.replies())
        {
            let _ = tool_manager.register(tools::ReplyToEventTool::new(self.global_root().to_path_buf()));
        }
        // A bot keeps procedures it will need again as its own skills.
        if let (None, Some(root), true) = (
            tool_filter,
            self.skill_catalog.install_roots().0,
            self.skill_catalog.is_bot_catalog(),
        ) {
            let _ = tool_manager.register(tools::SaveSkillTool::new(root));
        }

        // Computer use: drive other apps' windows through the accessibility API. Registered on
        // every platform -- the non-macOS build answers "unsupported" honestly instead of hiding
        // the tools, so the tool table (and therefore the prompt prefix) is identical everywhere.
        if !conversation {
            let _ = tool_manager.register(tools::computer_apps::tool());
            let _ = tool_manager.register(tools::computer_snapshot::tool());
            let _ = tool_manager.register(tools::computer_act::tool());
            let _ = tool_manager.register(tools::computer_key::tool());
        }

        // Skill runtime (application-layer capability): the catalog is read-only and shared across sessions; the activated set is **created fresh per session**
        // (activation state is isolated per session). `activate_skill` writes, `SkillContextProvider` reads,
        // sharing the same `Arc`, exactly like the runtime dispatch of todo / memory.
        let skill_catalog = Arc::new(self.skill_catalog.clone());
        let skill_runtime = Arc::new(SkillRuntime::new(Arc::clone(&skill_catalog)));
        let _ = tool_manager.register(tools::ActivateSkillTool::new(Arc::clone(&skill_runtime)));
        let _ = tool_manager.register(tools::ReadSkillFileTool::new(skill_catalog));

        // The available tool names (before adding `subagent`), for the child-tool allow/deny check and
        // for the child tool-table filter. `subagent` is deliberately absent (recursion guard).
        let available_tools: Vec<String> = tool_manager
            .definitions()
            .iter()
            .map(|definition| definition.name.clone())
            .collect();

        let subagents = match tool_filter {
            Some(selection) => {
                // Child: filter the table; never register `subagent`.
                apply_tool_filter(&mut tool_manager, selection);
                None
            }
            None => {
                if self.subagent_enabled && !conversation {
                    // Top level: create the manager (holding this factory) and expose the `subagent` tool.
                    let mut manager = SubagentManager::new(
                        Arc::clone(self),
                        self.subagent_limits.clone(),
                        available_tools,
                    );
                    manager.set_default_tools(self.subagent_default_tools.clone());
                    manager.set_default_timeout_ms(self.subagent_default_timeout_ms);
                    let manager = Arc::new(manager);
                    let _ = tool_manager.register(SubagentTool::new(Arc::clone(&manager)));
                    Some(manager)
                } else {
                    // Disabled by config: the `subagent` tool is not registered at all.
                    None
                }
            }
        };

        // The SDK `Agent` accepts only a **single** `context_provider`, but here we need to inject several
        // (task ledger + memory + skills) -- use `CompositeContextProvider` to concatenate in order.
        let mut providers: Vec<Arc<dyn ContextProvider>> = Vec::new();
        if !conversation {
            providers.push(Arc::new(TodoContextProvider::new(Arc::clone(&todo_store))));
        }
        providers.extend([
            Arc::new(MemoryContextProvider::new(Arc::clone(&memory))) as Arc<dyn ContextProvider>,
            // Skills (Tier1 catalog + Tier2 activated text): appended at the end of each round, KV-friendly.
            Arc::new(SkillContextProvider::new(Arc::clone(&skill_runtime)))
                as Arc<dyn ContextProvider>,
        ]);
        // A bot's conversation (not a worker, not a subagent) can dispatch tasks and is reminded
        // of them every turn. Registered after `available_tools` was taken, so subagents never
        // inherit these tools.
        if conversation && let Some(desk) = &self.tasks {
            let bot = self.bot_spec.name.clone();
            let _ = tool_manager.register(tools::DispatchTaskTool::new(desk.clone(), bot.clone()));
            let _ = tool_manager.register(tools::TaskNoteTool::new(desk.clone(), bot.clone()));
            providers.push(Arc::new(crate::pool::desk::TaskContextProvider::new(
                desk.clone(),
                bot,
            )));
        }
        let provider = CompositeContextProvider::new(providers);

        // Subagent guidance is injected only for the top-level agent when the tool is registered:
        // a child has no `subagent` tool (structural recursion guard), so it must not be told about one.
        // A test factory may set a fixed `SystemPrompt::from("test")`; only re-derive for the real
        // function-form prompt built by `assemble`.
        let system_prompt = if conversation {
            self.system_prompt_for(prompt::Role::Conversation)
        } else if tool_filter.is_none() && self.subagent_enabled {
            self.system_prompt.clone()
        } else {
            self.system_prompt_for(prompt::Role::Worker { subagents: false })
        };

        let agent = Agent::builder()
            .model_config(self.default_model_config()?)
            .system_prompt(system_prompt)
            .working_dir(self.workspace.clone())
            .compaction_instruction(COMPRESSION_INSTRUCTION)
            .compaction_config(self.compaction_config.clone())
            .max_steps(max_steps.unwrap_or(self.max_steps))
            .context_provider(Arc::new(provider) as Arc<dyn ContextProvider>)
            // Image references (`file_id`) become `data:` URLs only in the request view, so a
            // picture the model asks for never gets baked into the session log.
            .image_resolver({
                let store = Arc::clone(&self.artifacts);
                Arc::new(move |id: &str| store.data_url(id))
                    as Arc<dyn Fn(&str) -> Option<String> + Send + Sync>
            })
            .tools(tool_manager)
            .messages(messages)
            .build()?;
        Ok(BuiltAgent {
            agent,
            todo: todo_store,
            memory,
            subagents,
        })
    }

    /// Compute a full [`ModelConfig`] from a model entry (`docs/tool-calling-refactor.md` Step 3).
    ///
    /// This is the single entry point for "one-shot model config": the "model value + tool protocol" are computed together,
    /// and the caller hands the whole thing to [`shirley_agent_sdk::Agent::set_model_config`], **without having to remember**
    /// "first `set_model`, then `set_tool_calling`". The protocol is chosen by the model-declared `tool_mode`,
    /// falling back to the config-layer default (`self.model_config.tool_calling`) when undeclared.
    pub fn model_config_for(&self, entry: &ModelEntry) -> ModelConfig {
        if let Some(configured) = &entry.configured {
            return configured.config();
        }
        let mut config = self.model_config.clone();
        config.model.clone_from(&entry.value);
        config.tool_calling =
            resolve_tool_calling_for(entry.tool_mode.as_deref(), config.tool_calling);
        // Image capability is a catalog-declared fact, same shape as `tool_mode`: when the
        // entry declares it, the per-model declaration wins; when it does not (`None`), the
        // global `[provider] supports_images` (already in `config`) is kept.
        if let Some(supports_images) = entry.supports_images {
            config.supports_images = supports_images;
        }
        config
    }

    /// Derive the factory for one bot from the platform factory.
    ///
    /// What is per bot: its identity block in the system prompt, its tool table, its skill
    /// subset, its own session log directory (`bots/<name>/sessions`) and its private memory
    /// root (`bots/<name>/memory`, read merged with the shared root, written for bot-scoped
    /// entries). Everything else -- model config, catalogs, the media library, the embedder --
    /// is shared by `Arc`, so this is cheap and does no I/O.
    pub fn for_bot(&self, spec: crate::bot::BotSpec) -> Self {
        let bot_root = self.global_root().join("bots").join(&spec.name);
        let session_catalog: Arc<dyn SessionCatalog> =
            Arc::new(session::FileSessionCatalog::new(&bot_root));
        let memory_roots = vec![self.memory_roots[0].clone(), bot_root.join("memory")];
        let workspace = bot_root.join("workspace");
        // The bot's own skills (installed from the market, or written by the bot) come first and
        // win a name clash; then the platform's user-level skills `spec.skills` allows.
        let shared = self
            .skill_catalog
            .skills()
            .into_iter()
            .filter(|skill| spec.skills.is_empty() || spec.skills.contains(&skill.name))
            .collect();
        let skill_catalog = SkillCatalog::for_bot(skills::market::installed_root(&bot_root), shared);
        let system_prompt = prompt::build(
            workspace.clone(),
            prompt::Role::Worker {
                subagents: self.subagent_enabled,
            },
            spec.system_prompt_extra.clone(),
        );
        Self {
            model_config: self.model_config.clone(),
            system_prompt,
            compaction_config: self.compaction_config.clone(),
            max_steps: self.max_steps,
            model_catalog: Arc::clone(&self.model_catalog),
            session_catalog,
            needs_login: self.needs_login,
            memory_roots,
            curator_settings: self.curator_settings.clone(),
            consolidate_after_entries: self.consolidate_after_entries,
            embedder: self.embedder.clone(),
            artifacts: Arc::clone(&self.artifacts),
            skill_catalog,
            subagent_enabled: self.subagent_enabled,
            subagent_limits: self.subagent_limits.clone(),
            subagent_default_tools: self.subagent_default_tools.clone(),
            subagent_default_timeout_ms: self.subagent_default_timeout_ms,
            bot_spec: spec,
            workspace,
            tasks: self.tasks.clone(),
        }
    }

    /// Where the bot works (`bots/<name>/workspace`).
    pub fn workspace(&self) -> &std::path::Path {
        &self.workspace
    }

    /// The application root (`~/.shirley`): the parent of the shared memory root.
    pub fn global_root(&self) -> &std::path::Path {
        self.memory_roots[0]
            .parent()
            .expect("memory root has a parent")
    }

    pub fn model_providers_root(&self) -> &std::path::Path {
        self.global_root()
    }

    pub fn default_model_config(&self) -> Result<ModelConfig, AgentError> {
        crate::model_providers::default_config(self.model_providers_root())
            .map(|config| config.unwrap_or_else(|| self.model_config.clone()))
            .map_err(|error| AgentError::Other(Box::new(std::io::Error::other(error))))
    }

    /// Prewarm the model catalog: fetch the list once in the background so that the **first** open of `/model` also hits the cache.
    ///
    /// Does not block startup; failures are silent -- `RemoteCatalog` itself falls back to the fallback list.
    /// When not called inside a tokio runtime (e.g. unit tests), skip directly: prewarming is just an optimization, not required.
    pub fn prewarm_model_catalog(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let catalog = Arc::clone(&self.model_catalog);
        handle.spawn(async move {
            let _ = catalog.list().await;
        });
    }

    /// This factory's memory store (global + workspace multi-root; reads merge, writes land in the primary root).
    pub fn memory_store(&self) -> MemoryStore {
        MemoryStore::with_roots(self.memory_roots.clone())
    }

    /// Trigger one incremental curation at session end (`docs/memory.md` §5.1).
    ///
    /// Uses a **minimal `Agent`** (only borrowing the model config, without tools / system prompt / memory injection) to call
    /// [`Agent::complete`] -- one-shot, non-streaming, touching no session working set. The extracted candidates go through
    /// a deterministic self-check before being merged, and `index.md` is rebuilt.
    ///
    /// Read-only environment probing (§5.4) is not done in V1, honestly degrading to pure-trajectory curation.
    pub async fn curate(
        &self,
        messages: &[Message],
        session_ref: &str,
        session_model: &ModelConfig,
    ) -> Result<CurateOutcome, CuratorError> {
        let agent = self.curator_agent(session_model)?;
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

    /// Periodic consolidation (sleep learning, `docs/memory.md` §5.2): full scan -> model proposal -> self-check -> merge.
    ///
    /// Shares the curator model with [`AgentFactory::curate`] (if `[memory]` configures one, use the separate model).
    /// When `sessions_dir` exists, does **verification against original evidence**.
    pub async fn consolidate(
        &self,
        session_model: &ModelConfig,
    ) -> Result<memory::ConsolidateOutcome, CuratorError> {
        let agent = self.curator_agent(session_model)?;
        let store = self.memory_store();
        let sessions_dir = self.sessions_dir();
        let sessions_dir = sessions_dir.exists().then_some(sessions_dir.as_path());
        memory::consolidate(&agent, &store, sessions_dir).await
    }

    /// Whether the current memory entry count reaches the periodic-consolidation threshold (`[memory] consolidate_after_entries`).
    ///
    /// A threshold of `0` means do not auto-trigger. A failed directory read is treated as "do not trigger" (best-effort).
    pub fn should_consolidate(&self) -> bool {
        if self.consolidate_after_entries == 0 {
            return false;
        }
        self.memory_store()
            .list_entries()
            .is_ok_and(|entries| entries.len() >= self.consolidate_after_entries)
    }

    /// Build the **minimal `Agent`** used by the curator: only borrows the model config, without tools / system prompt / memory injection.
    fn curator_agent(&self, session_model: &ModelConfig) -> Result<Agent, CuratorError> {
        let config = build_curator_config(&self.curator_settings, session_model)
            .unwrap_or_else(|| session_model.clone());
        Agent::builder()
            .model_config(config)
            .build()
            .map_err(|error| CuratorError::Model(error.to_string()))
    }

    /// The session log dir (for the curator's evidence verification).
    fn sessions_dir(&self) -> PathBuf {
        self.session_catalog
            .storage_dir()
            .expect("factory catalog has a session storage directory")
    }
}

/// Resolve the tool transport mode from config (`docs/tool-calling-refactor.md`).
///
/// `tool_mode = "content"` -> [`ToolCallingMode::Tagged`]; everything else (including no statement)
/// -> [`ToolCallingMode::Native`] (structured function calling).
fn resolve_tool_calling(settings: &Settings) -> ToolCallingMode {
    resolve_tool_calling_for(settings.tool_mode.as_deref(), ToolCallingMode::Native)
}

/// Resolve a "model-declared `tool_mode`" into a [`ToolCallingMode`], falling back to `fallback` when undeclared.
///
/// This is the **single resolution entry point** for "auto-select the protocol by model declaration": `model_config_for` uses it,
/// so users do not have to configure manually every time they switch models.
///
/// Priority (model-first): `content` -> `Tagged`; `native` -> `Native`;
/// undeclared (`None` / empty string / other values) -> fall back to `fallback` (config-layer default, `Native` by default).
///
/// Vendor special-casing (e.g. Figma Make) is not here -- capability declarations belong to **model catalog data**
/// (`ModelEntry::tool_mode`, see `crate::models`).
pub fn resolve_tool_calling_for(
    model_tool_mode: Option<&str>,
    fallback: ToolCallingMode,
) -> ToolCallingMode {
    let mode = model_tool_mode
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(str::to_ascii_lowercase);
    match mode.as_deref() {
        Some("content") => ToolCallingMode::Tagged,
        Some("native") => ToolCallingMode::Native,
        // Undeclared / unrecognized value: fall back to the config-layer default, do not guess.
        _ => fallback,
    }
}

/// Build the curator's separate model config from the `[memory]` config; returns `None` when no curator endpoint/model is configured
/// (the curator falls back to the main model).
///
/// Inherits the main config's `stream` / `thinking` / `reasoning_effort` / `context_window_tokens`
/// and other fields, overriding only the endpoint / key / model / protocol explicitly given by the curator -- avoiding "configured a curator model
/// but lost the main config's other fields".
fn build_curator_config(
    mem: &crate::settings::MemorySettings,
    main: &ModelConfig,
) -> Option<ModelConfig> {
    // If no curator key is configured at all, do not build a separate config.
    if mem.curator_model.is_none()
        && mem.curator_base_url.is_none()
        && mem.curator_api_key.is_none()
        && mem.curator_protocol.is_none()
    {
        return None;
    }
    let mut config = main.clone();
    if let Some(model) = mem.curator_model.as_ref().filter(|m| !m.trim().is_empty()) {
        config.model.clone_from(model);
    }
    if let Some(base_url) = mem
        .curator_base_url
        .as_ref()
        .filter(|u| !u.trim().is_empty())
    {
        if mem.curator_api_key.is_none()
            && url::Url::parse(base_url).ok().map(|u| u.origin())
                != url::Url::parse(&main.base_url).ok().map(|u| u.origin())
        {
            config.api_key = None;
        }
        config.base_url.clone_from(base_url);
    }
    if let Some(api_key) = mem.curator_api_key.clone() {
        config.api_key = (!api_key.is_empty()).then_some(api_key);
    }
    if let Some(protocol) = mem.curator_protocol.as_deref()
        && let Ok(parsed) = crate::settings::parse_protocol_name(protocol)
    {
        config.protocol = parsed;
    }
    Some(config)
}

/// Build the embedding client from the `[memory]` config (the semantic leg of V2.5 hybrid retrieval).
///
/// Enabled only when both `embedding_base_url` + `embedding_model` are given; the key may be omitted (local services
/// often have no auth). Missing either -> `None` (retrieval degrades to pure BM25).
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
    let api_key = mem.embedding_api_key.clone().filter(|key| !key.is_empty());
    let config = EmbeddingConfig {
        endpoint: endpoint.to_string(),
        api_key,
        model: model.to_string(),
    };
    Embedder::new(config).ok().map(Arc::new)
}

/// The output of [`AgentFactory::build_agent`]: the `Agent` and its companion memory runtime handle.
pub struct BuiltAgent {
    pub agent: Agent,
    /// Task ledger (one per session): shares the same `Arc` as the `todo` tool / `TodoContextProvider`,
    /// so the interface can read task progress in real time (`TodoStore::view`) and render the task card.
    pub todo: Arc<TodoStore>,
    /// Memory runtime (one per session): the driver `set_query`s before starting a round, and hands it to the curator at session end.
    pub memory: Arc<MemoryRuntime>,
    /// Subagent manager (one per session, **top level only**; `None` for a child `Agent`).
    ///
    /// Shares the same `Arc` as the `subagent` tool, so the interface can read subagent status
    /// (`docs/subagent.md` P0). `None` when this `Agent` was built as a child.
    pub subagents: Option<Arc<SubagentManager>>,
}

/// Concatenate multiple [`ContextProvider`]s into one: joins each `context()` in order; all-empty returns `None`.
///
/// The SDK `Agent` accepts only a single `context_provider`, whereas the application layer wants to inject the task ledger and memory together
/// -- this is pure application-layer glue, not in the SDK.
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

    fn compaction_context(&self) -> Option<String> {
        let parts: Vec<String> = self
            .providers
            .iter()
            .filter_map(|p| p.compaction_context())
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }
}

/// Build a `ToolManager` with capability tools registered according to `spec`.
///
/// Infrastructure tools (todo / recall / computer_* / skill) are registered separately in
/// `build_agent_with` because they require per-session state (Arc<TodoStore> etc.).
/// Here we register the stateless "capability tools" that a `BotSpec` may restrict.
fn assemble_tools(artifacts: &ArtifactStore, spec: &crate::bot::BotSpec) -> ToolManager {
    use crate::bot::platform_tools as pt;
    let mut tool_manager = ToolManager::new();
    // The artifact jail is a `ToolManager` context value used by `read` and `media`.
    tool_manager.context_mut().insert(artifacts.clone());

    let want = |name: &str| !spec.has_tool_restriction() || spec.tools.iter().any(|t| t == name);

    if want(pt::BASH) {
        let _ = tool_manager.register(tools::bash_tool::tool());
    }
    if want(pt::GREP) {
        let _ = tool_manager.register(tools::grep_tool::tool());
    }
    if want(pt::READ) {
        let _ = tool_manager.register(tools::read_tool::tool());
    }
    if want(pt::MEDIA) {
        let _ = tool_manager.register(tools::MediaTool::new(artifacts.clone()));
    }
    if want(pt::WEB_SEARCH)
        && let Err(error) = tool_manager.register(tools::web_search_tool::tool())
    {
        eprintln!("[web_search] not enabled: {error}");
    }
    tool_manager
}

/// Trim a `ToolManager` down to a child's tool table (`docs/subagent.md` decision 5):
/// `(allow ?? all) - deny - {subagent}`. Unknown names are validated earlier (the `subagent` tool
/// returns `ArgumentsError`); here we only subtract.
fn apply_tool_filter(manager: &mut ToolManager, selection: &ToolSelection) {
    let names: Vec<String> = manager
        .definitions()
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    let keep: std::collections::HashSet<String> =
        filtered_tool_names(&names, selection).into_iter().collect();
    for name in names {
        if !keep.contains(&name) {
            let _ = manager.unregister(&name);
        }
    }
}

/// The child tool table as a pure function (unit-testable without a `ToolManager`).
///
/// `(allow ?? all) - deny - {subagent}`: the recursion guard (`subagent` is never delegated) is
/// applied here, structurally, regardless of what the caller allowed.
pub(crate) fn filtered_tool_names(names: &[String], selection: &ToolSelection) -> Vec<String> {
    names
        .iter()
        .filter(|name| name.as_str() != "subagent")
        .filter(|name| match &selection.allow {
            Some(allow) => allow.contains(name),
            None => true,
        })
        .filter(|name| !selection.deny.contains(name))
        .cloned()
        .collect()
}

/// Test-only factory constructor, shared by the bootstrap tests and the subagent tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::models::StaticCatalog;
    use crate::session::FileSessionCatalog;
    use shirley_agent_sdk::ModelProtocol;

    #[expect(
        clippy::needless_pass_by_value,
        reason = "test helper takes ownership to build owned factory fields"
    )]
    pub(crate) fn test_factory_with_base(
        root: std::path::PathBuf,
        base_url: &str,
        stream: bool,
    ) -> AgentFactory {
        AgentFactory {
            model_config: ModelConfig::builder()
                .protocol(ModelProtocol::ChatCompletions)
                .base_url(base_url)
                .model("test")
                .stream(stream)
                .build(),
            artifacts: Arc::new(ArtifactStore::new(std::env::temp_dir())),
            system_prompt: SystemPrompt::from("test"),
            compaction_config: CompactionConfig::default(),
            max_steps: 64,
            model_catalog: Arc::new(StaticCatalog::builtin()) as Arc<dyn ModelCatalog>,
            session_catalog: Arc::new(FileSessionCatalog::new(&root)) as Arc<dyn SessionCatalog>,
            needs_login: false,
            memory_roots: vec![root.join("memory")],
            curator_settings: crate::settings::MemorySettings::default(),
            consolidate_after_entries: 0,
            embedder: None,
            skill_catalog: SkillCatalog::new(Vec::new()),
            subagent_enabled: true,
            subagent_limits: SubagentLimits::default(),
            subagent_default_tools: None,
            subagent_default_timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
            bot_spec: crate::bot::BotSpec::default_bot(),
            workspace: root.join("workspace"),
            tasks: None,
        }
    }

    pub(crate) fn test_factory(root: std::path::PathBuf) -> AgentFactory {
        test_factory_with_base(root, "http://localhost", false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::StaticCatalog;
    use crate::session::FileSessionCatalog;
    use shirley_agent_sdk::ModelProtocol;

    #[expect(
        clippy::needless_pass_by_value,
        reason = "test helper takes ownership to build owned factory fields"
    )]
    fn factory(root: PathBuf) -> AgentFactory {
        AgentFactory {
            model_config: ModelConfig::builder()
                .protocol(ModelProtocol::ChatCompletions)
                .base_url("http://localhost")
                .model("test")
                .build(),
            artifacts: Arc::new(ArtifactStore::new(std::env::temp_dir())),
            system_prompt: SystemPrompt::from("test"),
            compaction_config: CompactionConfig::default(),
            max_steps: 64,
            model_catalog: Arc::new(StaticCatalog::builtin()) as Arc<dyn ModelCatalog>,
            session_catalog: Arc::new(FileSessionCatalog::new(&root)) as Arc<dyn SessionCatalog>,
            needs_login: false,
            memory_roots: vec![root.join("memory")],
            curator_settings: crate::settings::MemorySettings::default(),
            consolidate_after_entries: 0,
            embedder: None,
            skill_catalog: SkillCatalog::new(Vec::new()),
            subagent_enabled: true,
            subagent_limits: SubagentLimits::default(),
            subagent_default_tools: None,
            subagent_default_timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
            bot_spec: crate::bot::BotSpec::default_bot(),
            workspace: root.join("workspace"),
            tasks: None,
        }
    }

    #[tokio::test]
    async fn a_conversation_can_dispatch_but_a_worker_cannot() {
        let root =
            std::env::temp_dir().join(format!("shirley-desk-factory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let pool = Arc::new(tokio::sync::Mutex::new(crate::pool::TaskPool::open(
            root.join("tasks.json"),
        )));
        let (wake, _rx) = tokio::sync::mpsc::unbounded_channel();
        let factory = Arc::new(
            test_support::test_factory(root.clone())
                .with_tasks(crate::pool::desk::TaskDesk::new(pool, wake)),
        );
        let mut conversation = factory.build_agent(Vec::new()).unwrap().agent;
        assert!(conversation.unregister_tool("dispatch_task").is_ok());
        assert!(conversation.unregister_tool("task_note").is_ok());
        // The conversation talks; the worker machinery is not in its table.
        for tool in ["todo", "subagent", "computer_snapshot"] {
            assert!(conversation.unregister_tool(tool).is_err(), "{tool}");
        }
        let mut worker = factory.build_worker_agent(Vec::new()).unwrap().agent;
        assert!(worker.unregister_tool("dispatch_task").is_err());
        assert!(worker.unregister_tool("task_note").is_err());
        assert!(worker.unregister_tool("todo").is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn grep_and_read_are_registered_and_available_to_children() {
        let manager = assemble_tools(
            &ArtifactStore::new(std::env::temp_dir()),
            &crate::bot::BotSpec::default_bot(),
        );
        let names: Vec<String> = manager
            .definitions()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        assert!(names.iter().any(|name| name == "grep"));
        assert!(names.iter().any(|name| name == "read"));
        assert!(names.iter().all(|name| name != "read_file"));
        assert!(
            filtered_tool_names(&names, &ToolSelection::default())
                .iter()
                .any(|name| name == "grep")
        );
        assert_eq!(
            filtered_tool_names(
                &names,
                &ToolSelection {
                    allow: Some(vec!["grep".into(), "read".into()]),
                    deny: vec![],
                }
            ),
            vec!["grep", "read"]
        );
    }

    #[test]
    fn child_tool_filter_applies_allow_deny_and_recursion_guard() {
        let names = vec![
            "bash".to_string(),
            "read".to_string(),
            "subagent".to_string(),
        ];
        // No selection: everything except `subagent` (the recursion guard is structural).
        assert_eq!(
            filtered_tool_names(&names, &ToolSelection::default()),
            vec!["bash", "read"]
        );
        // Allowlist keeps only the named tools.
        assert_eq!(
            filtered_tool_names(
                &names,
                &ToolSelection {
                    allow: Some(vec!["read".into()]),
                    deny: vec![],
                }
            ),
            vec!["read"]
        );
        // Denylist subtracts from the default set.
        assert_eq!(
            filtered_tool_names(
                &names,
                &ToolSelection {
                    allow: None,
                    deny: vec!["bash".into()],
                }
            ),
            vec!["read"]
        );
        // Even an explicit allow of `subagent` cannot get it back.
        assert!(
            filtered_tool_names(
                &names,
                &ToolSelection {
                    allow: Some(vec!["subagent".into()]),
                    deny: vec![],
                }
            )
            .is_empty()
        );
    }

    fn tmp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shirley_bootstrap_{tag}_{}", std::process::id()))
    }

    /// Live smoke: assemble from the **real workspace config** and run one round, verifying the endpoint / protocol / key work.
    ///
    /// Depends on an external service; ignored by default, run manually:
    //   cargo test --bin Shirley live_smoke_against_configured_provider -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "live smoke test; requires a configured external provider"]
    async fn live_smoke_against_configured_provider() {
        let factory = Arc::new(AgentFactory::assemble().expect("assembly failed"));
        let mut built = factory
            .build_agent(Vec::new())
            .expect("failed to build Agent");
        let result = built
            .agent
            .run("Reply with just two words: hello")
            .await
            .expect("request failed");
        let text = result
            .messages
            .iter()
            .filter_map(|m| match m {
                Message::Assistant { content, .. } => content.clone(),
                _ => None,
            })
            .collect::<String>();
        eprintln!("[smoke] model reply: {text:?}");
        assert!(!text.trim().is_empty(), "should get a non-empty reply");
    }

    /// An empty-impl provider, to verify `CompositeContextProvider`'s concatenation / short-circuit semantics.
    struct StaticProvider(Option<&'static str>);

    impl ContextProvider for StaticProvider {
        fn context(&self) -> Option<String> {
            self.0.map(str::to_owned)
        }
    }

    struct TaskProvider;
    impl ContextProvider for TaskProvider {
        fn context(&self) -> Option<String> {
            Some("normal task state".into())
        }
        fn compaction_context(&self) -> Option<String> {
            Some("task checkpoint".into())
        }
    }

    #[test]
    fn composite_compaction_context_only_includes_opted_in_state() {
        let provider = CompositeContextProvider::new(vec![
            Arc::new(StaticProvider(Some("full skills and memory"))),
            Arc::new(TaskProvider),
        ]);
        assert_eq!(
            provider.compaction_context().as_deref(),
            Some("task checkpoint")
        );
        assert!(
            provider
                .context()
                .unwrap()
                .contains("full skills and memory")
        );
    }

    /// Regression: `CompositeContextProvider` concatenates multiple providers' `context()` in order;
    /// returns `None` when all are empty (or the list is empty), injecting no empty content.
    #[test]
    fn composite_context_provider_concatenates_and_short_circuits() {
        let both = CompositeContextProvider::new(vec![
            Arc::new(StaticProvider(Some("ledger"))) as Arc<dyn ContextProvider>,
            Arc::new(StaticProvider(Some("memory"))) as Arc<dyn ContextProvider>,
        ]);
        assert_eq!(both.context().as_deref(), Some("ledger\n\nmemory"));

        // One side empty: keep only the side with content (must not produce an extra separator).
        let one = CompositeContextProvider::new(vec![
            Arc::new(StaticProvider(None)) as Arc<dyn ContextProvider>,
            Arc::new(StaticProvider(Some("memory"))) as Arc<dyn ContextProvider>,
        ]);
        assert_eq!(one.context().as_deref(), Some("memory"));

        // All empty / empty list: return `None`, never inject an empty string.
        let none = CompositeContextProvider::new(vec![
            Arc::new(StaticProvider(None)) as Arc<dyn ContextProvider>
        ]);
        assert_eq!(none.context(), None);
        assert_eq!(CompositeContextProvider::new(vec![]).context(), None);
    }

    /// Model declaration first: `content` / `native` override fallback; undeclared falls back to fallback.
    #[test]
    fn resolve_tool_calling_is_model_first() {
        let native = ToolCallingMode::Native;
        let content = ToolCallingMode::Tagged;

        // Model declares content -> content (even if fallback is native).
        assert!(resolve_tool_calling_for(Some("content"), native).is_content());
        // Model declares native -> native (even if fallback is content).
        assert!(!resolve_tool_calling_for(Some("native"), content).is_content());
        // Case / whitespace normalized.
        assert!(resolve_tool_calling_for(Some("  CONTENT "), native).is_content());
        // Undeclared / empty string / unrecognized -> fall back to fallback (verify both sides).
        assert!(!resolve_tool_calling_for(None, native).is_content());
        assert!(!resolve_tool_calling_for(Some(""), native).is_content());
        assert!(!resolve_tool_calling_for(Some("wat"), native).is_content());
        assert!(resolve_tool_calling_for(None, content).is_content());
        assert!(resolve_tool_calling_for(Some("wat"), content).is_content());
    }

    /// `model_config_for`: model value + tool protocol computed together (`docs/tool-calling-refactor.md` Step 3).
    #[test]
    fn model_config_for_applies_entry_tool_mode() {
        let f = factory(tmp_root("mcfg"));
        let tagged =
            ModelEntry::new("tagged", "tagged", "p").with_tool_mode(Some("content".into()));
        let plain = ModelEntry::new("plain", "plain", "p");

        let config = f.model_config_for(&tagged);
        assert_eq!(config.model, "tagged");
        assert!(
            config.tool_calling.is_content(),
            "declaring content should pick the Tagged protocol"
        );

        let config = f.model_config_for(&plain);
        assert_eq!(config.model, "plain");
        assert!(
            !config.tool_calling.is_content(),
            "undeclared should fall back to the config default Native"
        );
    }

    /// `model_config_for`: image capability is a catalog-declared fact, applied per model.
    /// Declared wins; undeclared keeps the global `[provider] supports_images`.
    #[test]
    fn model_config_for_applies_entry_supports_images() {
        let f = factory(tmp_root("mcfg_img"));
        // Baseline: the factory's global config has images off (deny by default).
        assert!(!f.model_config.supports_images);

        let with_images = ModelEntry::new("vision", "vision", "p").with_supports_images(Some(true));
        let without = ModelEntry::new("text", "text", "p").with_supports_images(Some(false));
        let undeclared = ModelEntry::new("plain", "plain", "p");

        assert!(
            f.model_config_for(&with_images).supports_images,
            "a model declaring images should turn the capability on"
        );
        assert!(
            !f.model_config_for(&without).supports_images,
            "a model explicitly declaring no images should turn the capability off"
        );
        assert!(
            !f.model_config_for(&undeclared).supports_images,
            "undeclared should keep the global config value"
        );
    }

    #[test]
    fn memory_curator_follows_session_provider_and_does_not_reuse_key_for_another_host() {
        let root = std::env::temp_dir().join(format!(
            "shirley-curator-provider-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mut factory = super::test_support::test_factory(root.clone());
        let session = ModelConfig::builder()
            .protocol(shirley_agent_sdk::ModelProtocol::Responses)
            .base_url("https://selected.example/v1/responses")
            .model("chosen")
            .api_key("selected-key")
            .build();
        let curator = factory.curator_agent(&session).unwrap();
        assert_eq!(curator.model_config().base_url, session.base_url);
        assert_eq!(curator.model_config().api_key, session.api_key);
        factory.curator_settings.curator_base_url =
            Some("https://different.example/v1/responses".into());
        assert_eq!(
            factory
                .curator_agent(&session)
                .unwrap()
                .model_config()
                .api_key,
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
