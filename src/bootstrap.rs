//! Application assembly (Bootstrap): decouples "where config comes from" from "which interface renders it".
//!
//! Before the desktop interface was introduced (`docs/desktop-interface.md`), the assembly logic was hardcoded in `main.rs`
//! and called `interface::run` directly after assembling. Now it is extracted into an [`AgentFactory`]:
//! **the TUI and the desktop interface share the same assembly output** (the same model config / tools / session catalog),
//! only the rendering differs.
//!
//! This is **one of the LCAs** in "sibling interfaces, shared LCA": the SDK's `Agent` is the other.
//! Both interfaces are its consumers; neither monopolizes assembly.
//!
//! Multi-session (`docs/multi-session.md` decision 5): assembly is no longer "build one `Agent`",
//! but produces an [`AgentFactory`] -- **a factory that can repeatedly build `Agent`s per session**.
//! Each session gets its own independent `Agent` via [`AgentFactory::build_agent`] (model config /
//! tool definitions are reused by the factory; `todo` / session logs are isolated per instance inside `Agent::new`),
//! thereby supporting parallel sessions.

use std::path::PathBuf;
use std::sync::Arc;

use shirley_agent_sdk::{
    Agent, AgentError, CompressionConfig, ContextProvider, Message, ModelConfig, SystemPrompt,
    ToolCallingMode, ToolManager,
};

use crate::memory::{
    self, CurateOutcome, CuratorError, Embedder, EmbeddingConfig, MemoryContextProvider,
    MemoryRuntime, MemoryStore,
};
use crate::models::{self, ModelCatalog, ModelEntry};
use crate::prompt;
use crate::session::{self, SessionCatalog};
use crate::settings::Settings;
use crate::skills::{
    self, SkillCatalog, SkillContextProvider, SkillRuntime,
};
use crate::subagent::{
    DEFAULT_MAX_CONCURRENT, DEFAULT_MAX_DEPTH, DEFAULT_PER_TASK_MAX_STEPS, DEFAULT_WAIT_TIMEOUT_MS,
    SubagentLimits, SubagentManager, SubagentTool, ToolSelection,
};
use crate::todo::{TodoContextProvider, TodoStore};
use crate::tools;

/// Compression instruction: shared by all sessions (does not vary by session). The text lives in `prompts/compression_instruction.md`.
const COMPRESSION_INSTRUCTION: &str = include_str!("../prompts/compression_instruction.md");

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
    compression_config: CompressionConfig,
    max_steps: usize,
    /// Working dir (workspace root). The desktop interface uses it for `@` file search (`workspace_search`);
    /// under a TUI-only build nobody reads it, so allow dead_code.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub working_dir: PathBuf,
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
}

impl AgentFactory {
    /// Assemble all runtime dependencies. The working dir is decided by [`crate::prompt::workspace_root`].
    pub fn assemble(working_dir: PathBuf) -> Result<Self, AgentError> {
        Self::assemble_at(working_dir, crate::storage::global_root())
    }

    /// Explicit application-data root for isolated tests and assembly.
    pub(crate) fn assemble_at(working_dir: PathBuf, global_root: PathBuf) -> Result<Self, AgentError> {
        let paths = crate::storage::StoragePaths::new(global_root, &working_dir)
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        paths.migrate().map_err(|error| AgentError::Other(Box::new(error)))?;
        let settings = Settings::load(Some(&paths.global.join("config.toml")), &|key| std::env::var(key).ok())
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        let needs_login = !settings.is_configured() && crate::model_providers::default_config(&paths.global)
            .map_err(|error| AgentError::Other(Box::new(std::io::Error::other(error))))?.is_none();

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
        let model_catalog: Arc<dyn ModelCatalog> = Arc::new(crate::model_providers::ConfiguredCatalog {
            global: paths.global.clone(), fallback: legacy_catalog,
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

        // Compression strategy: if config does not specify it, follow the SDK default (`CompressionConfig::default`).
        let compression_config = settings.compression.to_sdk();

        // All runtime data lives globally; canonical project identity isolates sessions.
        let file_catalog = session::FileSessionCatalog::new(&paths.state);
        file_catalog
            .adopt_legacy()
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        let session_catalog: Arc<dyn SessionCatalog> = Arc::new(file_catalog);

        // The curator's separate model (V2-C): only if `[memory]` configures curator_* do we build a separate config,
        // otherwise `None` (falls back to the main model). Defaults inherit the main config's stream / thinking fields.
        let curator_settings = settings.memory.clone();

        // The embedding client for memory hybrid retrieval (V2.5): the semantic leg is enabled only with both endpoint + model;
        // otherwise `None` (retrieval degrades to pure BM25, never pretends there is a semantic leg).
        let embedder = build_embedder(&settings);

        // Skill catalog (`docs/skills.md` P0): scans project-level + user-level skills once at assembly,
        // read-only, shared across sessions; each session injects its Tier1 catalog via `SkillContextProvider`.
        let skill_catalog = SkillCatalog::discover(&working_dir, skills::user_home().as_deref());

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

        Ok(Self {
            model_config,
            system_prompt: prompt::build_with_subagents(working_dir.clone(), subagent_enabled),
            compression_config,
            max_steps: settings.max_steps,
            working_dir: working_dir.clone(),
            model_catalog,
            session_catalog,
            needs_login,
            memory_roots: paths.memory_roots(),
            curator_settings,
            consolidate_after_entries: settings.memory.consolidate_after_entries.unwrap_or(0),
            embedder,
            skill_catalog,
            subagent_enabled,
            subagent_limits,
            subagent_default_tools,
            subagent_default_timeout_ms,
        })
    }

    /// Build an independent `Agent` from the **restored message working set** (`docs/multi-session.md` decision 5).
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

    /// The system prompt for an `Agent` that has **no** `subagent` tool (a child, or the top level
    /// with subagents disabled). Re-derived from the working dir so the subagent guidance block is
    /// left out (it would be a factual lie where the tool does not exist). For the test factories
    /// that install a fixed prompt this still yields a valid (non-empty) system message.
    fn system_prompt_without_subagents(&self) -> SystemPrompt {
        prompt::build_with_subagents(self.working_dir.clone(), false)
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
        let mut tool_manager = assemble_tools();
        // Pin `read` / `grep` to this Agent's real workspace root. Without it they would fall
        // back to the `SHIRLEY_WORKSPACE` env var / process CWD, which is decoupled from the
        // Agent's `working_dir` (e.g. the desktop interface's multi-project mode: one process,
        // several project roots), so a relative path resolved against the wrong directory.
        tool_manager
            .context_mut()
            .insert(tools::read::WorkspaceRoot(self.working_dir.clone()));
        let _ = tool_manager.register(tools::TodoTool::new(todo_store.clone()));
        // Active memory recall tool (read-only): shares the same `MemoryRuntime` as automatic injection.
        // Memory writes go **through no** tool; they are done only by the background curator (`docs/memory.md` decision 3).
        let _ = tool_manager.register(tools::RecallTool::new(memory.clone()));

        // Skill runtime (application-layer capability): the catalog is read-only and shared across sessions; the activated set is **created fresh per session**
        // (activation state is isolated per session). `activate_skill` writes, `SkillContextProvider` reads,
        // sharing the same `Arc`, exactly like the runtime dispatch of todo / memory.
        let skill_catalog = Arc::new(self.skill_catalog.clone());
        let skill_runtime = Arc::new(SkillRuntime::new(skill_catalog.clone()));
        let _ = tool_manager.register(tools::ActivateSkillTool::new(skill_runtime.clone()));
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
                if self.subagent_enabled {
                    // Top level: create the manager (holding this factory) and expose the `subagent` tool.
                    let mut manager = SubagentManager::new(
                        self.clone(),
                        self.subagent_limits.clone(),
                        available_tools,
                    );
                    manager.set_default_tools(self.subagent_default_tools.clone());
                    manager.set_default_timeout_ms(self.subagent_default_timeout_ms);
                    let manager = Arc::new(manager);
                    let _ = tool_manager.register(SubagentTool::new(manager.clone()));
                    Some(manager)
                } else {
                    // Disabled by config: the `subagent` tool is not registered at all.
                    None
                }
            }
        };

        // The SDK `Agent` accepts only a **single** `context_provider`, but here we need to inject several
        // (task ledger + memory + skills) -- use `CompositeContextProvider` to concatenate in order.
        let provider = CompositeContextProvider::new(vec![
            Arc::new(TodoContextProvider::new(todo_store.clone())) as Arc<dyn ContextProvider>,
            Arc::new(MemoryContextProvider::new(memory.clone())) as Arc<dyn ContextProvider>,
            // Skills (Tier1 catalog + Tier2 activated text): appended at the end of each round, KV-friendly.
            Arc::new(SkillContextProvider::new(skill_runtime.clone())) as Arc<dyn ContextProvider>,
        ]);

        // Subagent guidance is injected only for the top-level agent when the tool is registered:
        // a child has no `subagent` tool (structural recursion guard), so it must not be told about one.
        // A test factory may set a fixed `SystemPrompt::from("test")`; only re-derive for the real
        // function-form prompt built by `assemble`.
        let system_prompt = if tool_filter.is_none() && self.subagent_enabled {
            self.system_prompt.clone()
        } else {
            self.system_prompt_without_subagents()
        };

        let agent = Agent::builder()
            .model_config(self.default_model_config()?)
            .system_prompt(system_prompt)
            .working_dir(self.working_dir.clone())
            .compression_instruction(COMPRESSION_INSTRUCTION)
            .compression_config(self.compression_config.clone())
            .max_steps(max_steps.unwrap_or(self.max_steps))
            .context_provider(Arc::new(provider) as Arc<dyn ContextProvider>)
            .tools(tool_manager)
            .messages(messages)
            .build()?;
        Ok(BuiltAgent {
            agent,
            todo: todo_store,
            memory,
            skills: skill_runtime,
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
        if let Some(configured) = &entry.configured { return configured.config(); }
        let mut config = self.model_config.clone();
        config.model = entry.value.clone();
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

    pub fn model_providers_root(&self) -> &std::path::Path {
        self.memory_roots[0].parent().expect("memory root has a parent")
    }

    pub fn default_model_config(&self) -> Result<ModelConfig, AgentError> {
        crate::model_providers::default_config(self.model_providers_root())
            .map(|config| config.unwrap_or_else(|| self.model_config.clone()))
            .map_err(|error| AgentError::Other(Box::new(std::io::Error::other(error))))
    }

    /// For `/login`: after changing the endpoint / key, recompute the model config (keep the current `model` if not filled).
    ///
    /// Fixes "`/login` does not re-determine the protocol": after switching from a content endpoint to another, the protocol follows back to
    /// the config default and does not linger (`docs/tool-calling-refactor.md` Step 3). The protocol does not follow `model`
    /// -- what `/login` fills is the "default model", whose declaration is unknown, so falling back to the config default is safest.
    pub fn model_config_for_login(
        &self,
        base_url: String,
        api_key: Option<String>,
        model: Option<String>,
    ) -> ModelConfig {
        let mut config = self.model_config.clone();
        config.base_url = base_url;
        config.api_key = api_key;
        if let Some(model) = model {
            config.model = model;
        }
        config
    }

    /// Prewarm the model catalog: fetch the list once in the background so that the **first** open of `/model` also hits the cache.
    ///
    /// Does not block startup; failures are silent -- `RemoteCatalog` itself falls back to the fallback list.
    /// When not called inside a tokio runtime (e.g. unit tests), skip directly: prewarming is just an optimization, not required.
    pub fn prewarm_model_catalog(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let catalog = self.model_catalog.clone();
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
    pub async fn consolidate(&self, session_model: &ModelConfig) -> Result<memory::ConsolidateOutcome, CuratorError> {
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
            .map(|entries| entries.len() >= self.consolidate_after_entries)
            .unwrap_or(false)
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
        self.session_catalog.storage_dir().expect("factory catalog has a session storage directory")
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
        .map(|mode| mode.to_ascii_lowercase());
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
fn build_curator_config(mem: &crate::settings::MemorySettings, main: &ModelConfig) -> Option<ModelConfig> {
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
        config.model = model.clone();
    }
    if let Some(base_url) = mem.curator_base_url.as_ref().filter(|u| !u.trim().is_empty()) {
        if mem.curator_api_key.is_none() && url::Url::parse(base_url).ok().map(|u| u.origin())
            != url::Url::parse(&main.base_url).ok().map(|u| u.origin()) {
            config.api_key = None;
        }
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

/// The output of [`AgentFactory::build_agent`]: the `Agent` and its companion memory runtime handle.
pub struct BuiltAgent {
    pub agent: Agent,
    /// Task ledger (one per session): shares the same `Arc` as the `todo` tool / `TodoContextProvider`,
    /// so the interface can read task progress in real time (`TodoStore::view`) and render the task card.
    pub todo: Arc<TodoStore>,
    /// Memory runtime (one per session): the driver `set_query`s before starting a round, and hands it to the curator at session end.
    pub memory: Arc<MemoryRuntime>,
    /// Skill runtime (one per session): the TUI's `/<skill-name>` slash entry activates directly via this handle
    /// (`docs/skills.md` 5.6). Shares the same `Arc` as `SkillContextProvider`.
    pub skills: Arc<SkillRuntime>,
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

/// Build a `ToolManager` with all tools registered (bash / grep / read are always registered;
/// web_search is not registered when unconfigured). Each `Agent` holds one; tool state is isolated per instance.
fn assemble_tools() -> ToolManager {
    let mut tool_manager = ToolManager::new();
    let _ = tool_manager.register(tools::bash_tool::tool());
    let _ = tool_manager.register(tools::grep_tool::tool());
    let _ = tool_manager.register(tools::read_tool::tool());
    if let Err(error) = tool_manager.register(tools::web_search_tool::tool()) {
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
    let keep: std::collections::HashSet<String> = filtered_tool_names(&names, selection)
        .into_iter()
        .collect();
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
            system_prompt: SystemPrompt::from("test"),
            compression_config: CompressionConfig::default(),
            max_steps: 64,
            working_dir: root.clone(),
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
        }
    }

    pub(crate) fn test_factory(root: std::path::PathBuf) -> AgentFactory {
        test_factory_with_base(root, "http://localhost", false)
    }
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
            max_steps: 64,
            working_dir: root.clone(),
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
        }
    }

    #[test]
    fn global_storage_restores_legacy_sessions_and_project_memory() {
        let root = std::env::temp_dir().join(format!("shirley-global-migration-{}", std::process::id()));
        let workspace = root.join("repo");
        let legacy = workspace.join(".shirley");
        std::fs::create_dir_all(legacy.join("sessions")).unwrap();
        std::fs::create_dir_all(legacy.join("memory/facts")).unwrap();
        let message = Message::User { content: "existing conversation".into() };
        std::fs::write(legacy.join("sessions/existing.jsonl"), format!("{}\n", serde_json::to_string(&message).unwrap())).unwrap();
        std::fs::write(legacy.join("sessions/existing.title"), "Existing title").unwrap();
        std::fs::write(legacy.join("memory/facts/project.md"), "---\nid: fact-project\ntype: fact\ndomain: project\nsubject: project\ncreated_at: 2026-10-16\nstatus: active\nconfidence: high\nsource:\n  - session: existing.jsonl#turn:1\n---\n\nExisting project fact.\n").unwrap();
        let factory = AgentFactory::assemble_at(workspace.clone(), root.join("global")).unwrap();
        let sessions = factory.session_catalog.list().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].label, "Existing title");
        assert_eq!(sessions[0].preview, "existing conversation");
        assert!(factory.sessions_dir().starts_with(root.join("global")));
        assert!(factory.memory_store().list_entries().unwrap().iter().any(|(_, entry)| entry.id == "fact-project"));
        let messages = factory.session_catalog.open("existing").unwrap().load().unwrap();
        assert_eq!(serde_json::to_value(messages).unwrap(), serde_json::to_value(vec![message]).unwrap());
        assert!(!legacy.exists());
        let _ = AgentFactory::assemble_at(workspace, root.join("global")).unwrap();
        assert!(!legacy.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grep_and_read_are_registered_and_available_to_children() {
        let manager = assemble_tools();
        let names: Vec<String> = manager.definitions().iter()
            .map(|tool| tool.name.clone()).collect();
        assert!(names.iter().any(|name| name == "grep"));
        assert!(names.iter().any(|name| name == "read"));
        assert!(names.iter().all(|name| name != "read_file"));
        assert!(filtered_tool_names(&names, &ToolSelection::default())
            .iter().any(|name| name == "grep"));
        assert_eq!(
            filtered_tool_names(&names, &ToolSelection {
                allow: Some(vec!["grep".into(), "read".into()]),
                deny: vec![],
            }),
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

    /// Regression: after deleting the **current** session, the foreground pointer must land on a fresh empty session, not dangle on the deleted one.
    ///
    /// Previously `agent_delete_session` only deleted from disk without touching `SessionManager`, so `active` still pointed at the deleted
    /// session, `agent_current_session` kept returning the old name, and the frontend opened it again -- the interface got stuck on the deleted session.
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
            None,
            None,
            None,
        );
        assert_eq!(manager.active_name(), Some("s"));

        manager.delete_session("s").unwrap();

        let active = manager.active_name().map(str::to_owned);
        assert_ne!(active.as_deref(), Some("s"), "the foreground should no longer sit on the deleted session");
        assert!(active.is_some(), "after deleting the foreground session a fresh empty one should be supplied");
        assert!(manager.active_agent().is_some(), "the new empty session should be ready");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Live smoke: assemble from the **real workspace config** and run one round, verifying the endpoint / protocol / key work.
    ///
    /// Depends on an external service; ignored by default, run manually:
    //   cargo test --bin Shirley live_smoke_against_configured_provider -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_smoke_against_configured_provider() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let factory = Arc::new(AgentFactory::assemble(root).expect("assembly failed"));
        let mut built = factory.build_agent(Vec::new()).expect("failed to build Agent");
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
            .collect::<Vec<_>>()
            .join("");
        eprintln!("[smoke] model reply: {text:?}");
        assert!(!text.trim().is_empty(), "should get a non-empty reply");
    }

    /// An empty-impl provider, to verify CompositeContextProvider's concatenation / short-circuit semantics.
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

    /// Regression: CompositeContextProvider concatenates multiple providers' `context()` in order;
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
        let tagged = ModelEntry::new("tagged", "tagged", "p")
            .with_tool_mode(Some("content".into()));
        let plain = ModelEntry::new("plain", "plain", "p");

        let config = f.model_config_for(&tagged);
        assert_eq!(config.model, "tagged");
        assert!(config.tool_calling.is_content(), "declaring content should pick the Tagged protocol");

        let config = f.model_config_for(&plain);
        assert_eq!(config.model, "plain");
        assert!(!config.tool_calling.is_content(), "undeclared should fall back to the config default Native");
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

    /// A running session must not be deleted (avoid deleting a log being written).
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
            None,
            None,
            None,
        );
        assert!(manager.begin_turn("s", "hello".into()).is_some());

        assert!(manager.delete_session("s").is_err(), "a running session should refuse deletion");
        assert_eq!(manager.active_name(), Some("s"), "the foreground should not change after a refusal");

        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn memory_curator_follows_session_provider_and_does_not_reuse_key_for_another_host() {
        let root = std::env::temp_dir().join(format!("shirley-curator-provider-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap()));
        let mut factory = super::test_support::test_factory(root.clone());
        let session = ModelConfig::builder().protocol(shirley_agent_sdk::ModelProtocol::Responses).base_url("https://selected.example/v1/responses").model("chosen").api_key("selected-key").build();
        let curator = factory.curator_agent(&session).unwrap();
        assert_eq!(curator.model_config().base_url, session.base_url);
        assert_eq!(curator.model_config().api_key, session.api_key);
        factory.curator_settings.curator_base_url = Some("https://different.example/v1/responses".into());
        assert_eq!(factory.curator_agent(&session).unwrap().model_config().api_key, None);
        let _ = std::fs::remove_dir_all(root);
    }

}
