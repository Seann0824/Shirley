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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use shirley_agent_sdk::{
    Agent, AgentError, CompressionConfig, ContextProvider, Message, ModelConfig, SystemPrompt,
    ToolCallingMode, ToolManager,
};

use crate::memory::{
    self, CurateOutcome, CuratorError, Embedder, EmbeddingConfig, MemoryContextProvider,
    MemoryRuntime, MemoryStore, RecallTool,
};
use crate::models::{self, ModelCatalog, ModelEntry};
use crate::prompt;
use crate::session::{self, SessionCatalog};
use crate::settings::Settings;
use crate::skills::{
    self, ActivateSkillTool, ReadSkillFileTool, SkillCatalog, SkillContextProvider, SkillRuntime,
};
use crate::todo::{TodoContextProvider, TodoStore, TodoTool};
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
    curator_model_config: Option<ModelConfig>,
    /// When the entry count reaches this threshold, trigger a periodic consolidation after curation (sleep learning). `0` = do not auto-trigger.
    consolidate_after_entries: usize,
    /// **V2.5** embedding client: constructed only when `[memory]` configures both `embedding_base_url` +
    /// `embedding_model` (`None` = pure BM25, honest degradation). Shared by all sessions.
    embedder: Option<Arc<Embedder>>,
    /// Skill catalog (`docs/skills.md` P0): scanned once at assembly, shared by all sessions (read-only, unchanging).
    /// Each `Agent` injects its Tier1 catalog at the end of each round via `SkillContextProvider`.
    skill_catalog: SkillCatalog,
}

/// The location of global memory relative to `config_dir` (cross-project).
const GLOBAL_MEMORY_REL: &str = "shirley/memory";
/// The location of workspace memory relative to the workspace root (travels with the repo).
const WORKSPACE_MEMORY_REL: &str = ".shirley/memory";

/// Memory root dirs: `[global, workspace]`. When `config_dir` is unavailable, degrades to workspace-only.
fn memory_roots(working_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(config_dir) = dirs::config_dir() {
        roots.push(config_dir.join(GLOBAL_MEMORY_REL));
    }
    roots.push(working_dir.join(WORKSPACE_MEMORY_REL));
    roots
}

impl AgentFactory {
    /// Assemble all runtime dependencies. The working dir is decided by [`crate::prompt::workspace_root`].
    pub fn assemble(working_dir: PathBuf) -> Result<Self, AgentError> {
        // Config loading (Plan A): priority `built-in defaults < global config.toml < workspace
        // .shirley/config.toml < env vars`.
        let settings = Settings::load_default(&working_dir)
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        let needs_login = !settings.is_configured();

        // Model catalog: by default derives `/v1/models` from the chat completions base_url,
        // but can be overridden with `models_url` in config. On fetch failure falls back to the built-in static list.
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
            .thinking(settings.thinking)
            .reasoning_effort(settings.reasoning_effort.clone())
            .maybe_extra_body(settings.extra_body.clone())
            // Tool transport mode (`docs/tool-calling-refactor.md`): if config says so, do it;
            // otherwise default to `Native`. The "per model declaration" when switching models is handled by `model_config_for`.
            .tool_calling(resolve_tool_calling(&settings))
            .build();
        model_config.context_window_tokens = Some(settings.context_window_tokens);

        // Compression strategy: if config does not specify it, follow the SDK default (`CompressionConfig::default`).
        let compression_config = settings.compression.to_sdk();

        // Session persistence: multi-session layout `<root>/.shirley/sessions/<name>.jsonl`.
        let file_catalog = session::FileSessionCatalog::new(&working_dir);
        file_catalog
            .adopt_legacy()
            .map_err(|error| AgentError::Other(Box::new(error)))?;
        let session_catalog: Arc<dyn SessionCatalog> = Arc::new(file_catalog);

        // The curator's separate model (V2-C): only if `[memory]` configures curator_* do we build a separate config,
        // otherwise `None` (falls back to the main model). Defaults inherit the main config's stream / thinking fields.
        let curator_model_config = build_curator_config(&settings, &model_config);

        // The embedding client for memory hybrid retrieval (V2.5): the semantic leg is enabled only with both endpoint + model;
        // otherwise `None` (retrieval degrades to pure BM25, never pretends there is a semantic leg).
        let embedder = build_embedder(&settings);

        // Skill catalog (`docs/skills.md` P0): scans project-level + user-level skills once at assembly,
        // read-only, shared across sessions; each session injects its Tier1 catalog via `SkillContextProvider`.
        let skill_catalog = SkillCatalog::discover(&working_dir, skills::user_home().as_deref());

        Ok(Self {
            model_config,
            system_prompt: prompt::build(working_dir.clone()),
            compression_config,
            max_steps: settings.max_steps,
            working_dir: working_dir.clone(),
            model_catalog,
            session_catalog,
            needs_login,
            memory_roots: memory_roots(&working_dir),
            curator_model_config,
            consolidate_after_entries: settings.memory.consolidate_after_entries.unwrap_or(0),
            embedder,
            skill_catalog,
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
    pub fn build_agent(&self, messages: Vec<Message>) -> Result<BuiltAgent, AgentError> {
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
        let _ = tool_manager.register(TodoTool::new(todo_store.clone()));
        // Active memory recall tool (read-only): shares the same `MemoryRuntime` as automatic injection.
        // Memory writes go **through no** tool; they are done only by the background curator (`docs/memory.md` decision 3).
        let _ = tool_manager.register(RecallTool::new(memory.clone()));

        // Skill runtime (application-layer capability): the catalog is read-only and shared across sessions; the activated set is **created fresh per session**
        // (activation state is isolated per session). `activate_skill` writes, `SkillContextProvider` reads,
        // sharing the same `Arc`, exactly like the runtime dispatch of todo / memory.
        let skill_catalog = Arc::new(self.skill_catalog.clone());
        let skill_runtime = Arc::new(SkillRuntime::new(skill_catalog.clone()));
        let _ = tool_manager.register(ActivateSkillTool::new(skill_runtime.clone()));
        let _ = tool_manager.register(ReadSkillFileTool::new(skill_catalog));

        // The SDK `Agent` accepts only a **single** `context_provider`, but here we need to inject several
        // (task ledger + memory + skills) -- use `CompositeContextProvider` to concatenate in order.
        let provider = CompositeContextProvider::new(vec![
            Arc::new(TodoContextProvider::new(todo_store.clone())) as Arc<dyn ContextProvider>,
            Arc::new(MemoryContextProvider::new(memory.clone())) as Arc<dyn ContextProvider>,
            // Skills (Tier1 catalog + Tier2 activated text): appended at the end of each round, KV-friendly.
            Arc::new(SkillContextProvider::new(skill_runtime.clone())) as Arc<dyn ContextProvider>,
        ]);

        let agent = Agent::builder()
            .model_config(self.model_config.clone())
            .system_prompt(self.system_prompt.clone())
            .working_dir(self.working_dir.clone())
            .compression_instruction(COMPRESSION_INSTRUCTION)
            .compression_config(self.compression_config.clone())
            .max_steps(self.max_steps)
            .context_provider(Arc::new(provider) as Arc<dyn ContextProvider>)
            .tools(tool_manager)
            .messages(messages)
            .build()?;
        Ok(BuiltAgent {
            agent,
            todo: todo_store,
            memory,
            skills: skill_runtime,
        })
    }

    /// Compute a full [`ModelConfig`] from a model entry (`docs/tool-calling-refactor.md` Step 3).
    ///
    /// This is the single entry point for "one-shot model config": the "model value + tool protocol" are computed together,
    /// and the caller hands the whole thing to [`shirley_agent_sdk::Agent::set_model_config`], **without having to remember**
    /// "first `set_model`, then `set_tool_calling`". The protocol is chosen by the model-declared `tool_mode`,
    /// falling back to the config-layer default (`self.model_config.tool_calling`) when undeclared.
    pub fn model_config_for(&self, entry: &ModelEntry) -> ModelConfig {
        let mut config = self.model_config.clone();
        config.model = entry.value.clone();
        config.tool_calling =
            resolve_tool_calling_for(entry.tool_mode.as_deref(), config.tool_calling);
        config
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

    /// Periodic consolidation (sleep learning, `docs/memory.md` §5.2): full scan -> model proposal -> self-check -> merge.
    ///
    /// Shares the curator model with [`AgentFactory::curate`] (if `[memory]` configures one, use the separate model).
    /// When `sessions_dir` exists, does **verification against original evidence**.
    pub async fn consolidate(&self) -> Result<memory::ConsolidateOutcome, CuratorError> {
        let agent = self.curator_agent()?;
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

    /// The session log dir (for the curator's evidence verification).
    fn sessions_dir(&self) -> PathBuf {
        self.working_dir.join(".shirley").join("sessions")
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
fn build_curator_config(settings: &Settings, main: &ModelConfig) -> Option<ModelConfig> {
    let mem = &settings.memory;
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

/// Build a `ToolManager` with all tools registered (bash / read_file are always registered;
/// web_search is not registered when unconfigured). Each `Agent` holds one; tool state is isolated per instance.
fn assemble_tools() -> ToolManager {
    let mut tool_manager = ToolManager::new();
    let _ = tool_manager.register(tools::bash_tool::tool());
    let _ = tool_manager.register(tools::read_file_tool::tool());
    if let Err(error) = tool_manager.register(tools::web_search_tool::tool()) {
        eprintln!("[web_search] not enabled: {error}");
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
            max_steps: 64,
            working_dir: root.clone(),
            model_catalog: Arc::new(StaticCatalog::builtin()) as Arc<dyn ModelCatalog>,
            session_catalog: Arc::new(FileSessionCatalog::new(&root)) as Arc<dyn SessionCatalog>,
            needs_login: false,
            memory_roots: vec![root.join(".shirley/memory")],
            curator_model_config: None,
            consolidate_after_entries: 0,
            embedder: None,
            skill_catalog: SkillCatalog::new(Vec::new()),
        }
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
        let factory = AgentFactory::assemble(root).expect("assembly failed");
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
        );
        assert!(manager.begin_turn("s", "hello".into()).is_some());

        assert!(manager.delete_session("s").is_err(), "a running session should refuse deletion");
        assert_eq!(manager.active_name(), Some("s"), "the foreground should not change after a refusal");

        let _ = std::fs::remove_dir_all(&root);
    }
}
