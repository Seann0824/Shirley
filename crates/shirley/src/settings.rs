//! Application-side config loading (Plan A: config file + env var overrides).
//!
//! Motivation (`plan.md`): for ordinary users, "edit `.env` to switch model services" is too programmer-y.
//! Decouple "where config comes from" from "how config is used" -- `main.rs` only consumes the final result,
//! no longer calling `std::env::var` directly.
//!
//! **This is an application-layer concern, not SDK**: the SDK only knows `ModelConfig`, and does not care whether it came from
//! an env var, a config file, or a wizard.
//!
//! # Priority (later overrides earlier)
//!
//! 1. Built-in defaults (protocol `ChatCompletions`, model `deepseek-v4.1-flash`,
//!    context window `104858 >> 1`) -- consistent with the old `main.rs` behavior.
//! 2. Global config: `~/.shirley/config.toml`.
//! 3. Env vars: `LOCAL_API_KEY` / `LOCAL_BASE_URL` / `LOCAL_MODELS_URL` /
//!    `LOCAL_MODEL` / `LOCAL_PROTOCOL` / `LOCAL_CONTEXT_WINDOW_TOKENS`.
//!
//! Env vars go last for compatibility with the existing `.env` habit: old users change nothing,
//! new users write a config file. Keep both; the migration is gradual.
//!
//! # Things that do not belong here
//!
//! `SHIRLEY_WORKSPACE` is **deliberately not absorbed**: it is "which project to run in this time" (session-level),
//! which is not the same class as "who the user is / which model service to use" (user-level). Mixing them makes the config file
//! non-portable across projects. The workspace root is still resolved separately by `prompt::workspace_root()`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shirley_agent_sdk::{CompressionConfig, ModelProtocol};

/// Built-in default model: consistent with the old hardcoding in `main.rs`; upgrading does not change default behavior.
const DEFAULT_MODEL: &str = "deepseek-v4.1-flash";
/// Built-in default context window: `104858 >> 1` (the value from the old `main.rs`).
const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 104_858 >> 1;
/// Built-in default reasoning effort (consistent with the old hardcoding in `bootstrap.rs`).
const DEFAULT_REASONING_EFFORT: &str = "low";

/// The structure of the config file (also the carrier for each override layer).
///
/// All fields are `Option`: `None` means "this layer does not manage this key", and merging yields to the lower-
/// priority layer. `deny_unknown_fields` makes a typo'd key an immediate error rather than being silently ignored
/// -- a misconfigured config should make noise, not pretend it was not seen.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileSettings {
    pub provider: ProviderSettings,
    /// Context compression strategy (the `[compression]` table in the config file).
    pub compression: CompressionSettings,
    /// Memory system config (the `[memory]` table in the config file).
    pub memory: MemorySettings,
    /// Execution limits for a single task.
    pub runtime: RuntimeSettings,
    /// Subagent (child Agent) config (the `[subagent]` table in the config file).
    pub subagent: SubagentSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeSettings {
    /// The model call limit per user task. Default 64. `0` means **no cap**
    /// (unlimited); the loop only ends when the model stops requesting tools.
    pub max_steps: Option<usize>,
}

/// Compression strategy config (the `[compression]` table in the config file).
///
/// All `Option`: `None` means "this layer does not manage this key", and merging yields to lower priority / built-in defaults.
/// The default value is the SDK's [`CompressionConfig::default`] (trigger `0.80` / retain `0.20` /
/// built-in summary template). Changing this only affects **when to compress, how much to retain, what the summary looks like**; domain-related
/// trade-offs still live in the `Agent`'s `compression_instruction` (`bootstrap.rs`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompressionSettings {
    /// Trigger threshold (as a ratio of the context window, `0 < r <= 1`). Default `0.80`.
    pub trigger_ratio: Option<f64>,
    /// The budget measure for retaining the tail (as a ratio of the context window, `0 < r < 1`). Default `0.20`.
    pub retain_ratio: Option<f64>,
    /// Summary output template (appended after `compression_instruction`). Defaults to the SDK template.
    pub template: Option<String>,
}

impl CompressionSettings {
    /// Merge into the SDK's [`CompressionConfig`]: `None` falls back to the SDK default.
    ///
    /// Invalid values (out-of-range `trigger_ratio`) are **clamped to the legal range** here rather than erroring:
    /// compression is a background capability, and a misconfiguration should not block startup; after clamping it degrades to default behavior.
    pub fn to_sdk(&self) -> CompressionConfig {
        let mut config = CompressionConfig::default();
        if let Some(ratio) = self.trigger_ratio.filter(|r| *r > 0.0 && *r <= 1.0) {
            config.trigger_ratio = ratio;
        }
        if let Some(ratio) = self.retain_ratio.filter(|r| *r > 0.0 && *r < 1.0) {
            config.retain_ratio = ratio;
        }
        if let Some(template) = self.template.as_ref().filter(|t| !t.trim().is_empty()) {
            config.template.clone_from(template);
        }
        config
    }

    /// Layer-by-layer override: a higher-priority `Some` beats a lower priority.
    fn merge_into(&mut self, over: CompressionSettings) {
        if over.trigger_ratio.is_some() {
            self.trigger_ratio = over.trigger_ratio;
        }
        if over.retain_ratio.is_some() {
            self.retain_ratio = over.retain_ratio;
        }
        if over.template.is_some() {
            self.template = over.template;
        }
    }
}

/// Memory system config (the `[memory]` table in the config file).
///
/// New in V2: allow configuring a **separate model** for the curator (cross-source review, `docs/memory.md` §10 / §6) --
/// the model that extracts / organizes memory is separate from the main chat model, reducing "reviewing yourself" bias. All `Option`:
/// if unconfigured, honestly falls back to the main model.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySettings {
    /// The model identifier used by the curator; defaults to falling back to the main model.
    pub curator_model: Option<String>,
    /// The curator's endpoint (full protocol address); defaults to falling back to the main endpoint.
    pub curator_base_url: Option<String>,
    /// The curator's key; defaults to falling back to the main key.
    pub curator_api_key: Option<String>,
    /// The curator's protocol name (`chat_completions` / `responses` / `anthropic_messages`);
    /// defaults to falling back to the main protocol.
    pub curator_protocol: Option<String>,
    /// When the entry count reaches this threshold, trigger a periodic consolidation after curation (sleep learning). Default `0` = do not auto-trigger.
    pub consolidate_after_entries: Option<usize>,
    /// **V2.5** embedding endpoint (OpenAI-compatible `POST .../embeddings`). Configure this
    /// and `embedding_model` in the global config so Chat and project sessions share semantic retrieval.
    /// Missing endpoint or model -> pure BM25; the API key is optional for local services.
    pub embedding_base_url: Option<String>,
    /// The embedding endpoint's key; defaults to treating it as "no auth" (common for local services).
    pub embedding_api_key: Option<String>,
    /// The embedding model identifier. The sidecar records it; a model change invalidates the whole old vector set.
    pub embedding_model: Option<String>,
}

impl MemorySettings {
    /// Layer-by-layer override: a higher-priority `Some` beats a lower priority.
    fn merge_into(&mut self, over: MemorySettings) {
        macro_rules! take {
            ($($field:ident),* $(,)?) => {
                $(if over.$field.is_some() { self.$field = over.$field; })*
            };
        }
        take!(
            curator_model,
            curator_base_url,
            curator_api_key,
            curator_protocol,
            consolidate_after_entries,
            embedding_base_url,
            embedding_api_key,
            embedding_model,
        );
    }
}

/// Subagent config (the `[subagent]` table in the config file, `docs/subagent.md` decision 8).
///
/// All `Option`: `None` means "this layer does not manage this key", and merging yields to lower priority / built-in defaults.
/// The caps are enforced by the application-layer `SubagentManager`; the SDK knows nothing about subagents.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubagentSettings {
    /// Master switch. `false` disables the `subagent` tool entirely (the tool is not registered).
    /// Default `true`.
    pub enabled: Option<bool>,
    /// Max subagents running at the same time; beyond this a dispatch is rejected outright. Default 4.
    pub max_concurrent: Option<usize>,
    /// Max nesting depth. `1` = a child cannot dispatch a child (default).
    pub max_depth: Option<usize>,
    /// A single child's model-call budget (independent of the parent). Default 24.
    pub per_task_max_steps: Option<usize>,
    /// Default `wait` / blocking-dispatch timeout in milliseconds. Default 600000 (10 min).
    pub default_timeout_ms: Option<u64>,
    /// The default tool set handed to a child when the model does not pass an explicit `tools`
    /// allowlist. `None` = all tools. `subagent` is never included (recursion guard).
    pub default_tools: Option<Vec<String>>,
}

impl SubagentSettings {
    /// Layer-by-layer override: a higher-priority `Some` beats a lower priority.
    fn merge_into(&mut self, over: SubagentSettings) {
        macro_rules! take {
            ($($field:ident),* $(,)?) => {
                $(if over.$field.is_some() { self.$field = over.$field; })*
            };
        }
        take!(
            enabled,
            max_concurrent,
            max_depth,
            per_task_max_steps,
            default_timeout_ms,
            default_tools,
        );
    }
}

/// Model service connection config (the `[provider]` table in the config file).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderSettings {
    /// Protocol name, see [`parse_protocol`].
    pub protocol: Option<String>,
    /// The full chat completions address, e.g. `http://127.0.0.1:8788/v1/chat/completions`.
    pub base_url: Option<String>,
    /// Key. When missing, treated as "no auth" (common for local services).
    pub api_key: Option<String>,
    /// The model list endpoint; when absent, derived from `base_url` (see `models::models_endpoint`).
    pub models_url: Option<String>,
    /// The model identifier used for requests.
    pub model: Option<String>,
    /// The context window token count.
    pub context_window_tokens: Option<u64>,
    /// Whether the model accepts image input. Default `false` (deny by default);
    /// the SDK rejects a request carrying an image unless this is explicitly on.
    pub supports_images: Option<bool>,
    /// Whether to enable thinking (reasoning). Default `true`.
    ///
    /// A real Anthropic-compatible endpoint requires `thinking` to also provide `budget_tokens`, whereas the adapter
    /// only writes `{"type":"enabled"}`; such endpoints need this set to `false`, or use `extra_body`
    /// to override the whole `thinking` object.
    pub thinking: Option<bool>,
    /// Reasoning effort (`low` / `medium` / `high`). Default `low`; deliberately ignored under the Anthropic protocol.
    pub reasoning_effort: Option<String>,
    /// An escape hatch to override / append to the request body (shallow merge, `null` deletes a key).
    /// See the SDK `ModelConfig::extra_body`; used to fill in vendor-private fields.
    pub extra_body: Option<toml::Value>,
    /// Tool transport mode: `"native"` (structured function calling) or `"content"` (text protocol).
    ///
    /// See the SDK `ToolCallingMode` and `docs/tool-calling-refactor.md`. Default (`None`) = no statement,
    /// resolved by `bootstrap` as "config > model declaration > default Native".
    pub tool_mode: Option<String>,
}

impl FileSettings {
    /// Parse from TOML text. Empty text is treated as an "empty config".
    pub fn parse(text: &str, path: &Path) -> Result<Self, SettingsError> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        toml::from_str(text).map_err(|e| SettingsError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    /// Write the current file settings back to `path` (atomic replacement: write a temp file then `rename`).
    ///
    /// Writing **overwrites the whole file** -- the caller should read existing content, change fields, then write back,
    /// otherwise other keys in the file are lost. Missing directories are created automatically.
    ///
    /// After writing, tighten file permissions to `0600` (owner read/write only): the config file may contain
    /// `api_key`, and the default `0644` would expose the key to other users on the same machine. A tightening failure is not treated as fatal
    /// -- some filesystems (e.g. certain mounted volumes) do not support chmod, and being able to write matters more.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| SettingsError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        restrict_permissions(path);
        Ok(())
    }

    /// Read the file; return `None` if it does not exist (missing config is not an error, it is "this layer said nothing").
    fn load_file(path: &Path) -> Result<Option<Self>, SettingsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(Self::parse(&text, path)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SettingsError::Io(e)),
        }
    }
}

/// The final merged config. Fields are all settled; `main.rs` consumes it directly.
///
/// Does not derive `Eq`: `compression.trigger_ratio` is `f64`, which only has `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub protocol: ModelProtocol,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models_url: Option<String>,
    pub model: String,
    pub context_window_tokens: u64,
    /// Whether the model accepts image input (default `false`).
    pub supports_images: bool,
    /// Whether to enable thinking (default `true`).
    pub thinking: bool,
    /// Reasoning effort (default `low`).
    pub reasoning_effort: String,
    /// Request body escape hatch (already converted to JSON, for the SDK to shallow-merge).
    pub extra_body: Option<serde_json::Value>,
    /// Tool transport mode (normalized lowercase): `"native"` / `"content"`. `None` = no statement.
    pub tool_mode: Option<String>,
    /// Compression strategy (already settled, can be handed to `Agent` via `to_sdk()` directly).
    pub compression: CompressionSettings,
    /// Memory system config (curator model / periodic consolidation threshold).
    pub memory: MemorySettings,
    /// Subagent config (caps / default tool set / default timeout).
    pub subagent: SubagentSettings,
    /// Model call limit per user task. `0` = unlimited (see [`RuntimeSettings::max_steps`]).
    pub max_steps: usize,
}

/// Config loading error.
///
/// Follows the project's unified display format `[prefix]: detail` (see `Agent.md`).
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("[config read failed]: {0}")]
    Io(#[from] std::io::Error),

    #[error("[config parse failed] {path}: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("[invalid config]: {0}")]
    Invalid(String),
}

impl Settings {
    /// Load config along the priority chain.
    ///
    /// `env` is the env var reader (injected for testing, does not touch global env).
    /// When `global_path` is `None`, skip the global layer (e.g. `config_dir` is unavailable).
    pub fn load(
        global_path: Option<&Path>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, SettingsError> {
        let global = match global_path {
            Some(path) => FileSettings::load_file(path)?,
            None => None,
        };
        let env_layer = env_layer(env);

        // Lowest priority first, overridden layer by layer. The file layers take their `[provider]` table; the env layer is already a table.
        let merged = merge_all([global.as_ref().map(|s| s.provider.clone()), Some(env_layer)]);

        // The compression strategy is likewise overridden layer by layer (the env layer has no such key, only the global file layer).
        let mut compression = CompressionSettings::default();
        for layer in [&global].into_iter().flatten() {
            compression.merge_into(layer.compression.clone());
        }

        // The memory config is likewise overridden layer by layer (the env layer has no such key, only the global file layer).
        let mut memory = MemorySettings::default();
        for layer in [&global].into_iter().flatten() {
            memory.merge_into(layer.memory.clone());
        }

        // The subagent config is likewise overridden layer by layer (the env layer has no such key).
        let mut subagent = SubagentSettings::default();
        for layer in [&global].into_iter().flatten() {
            subagent.merge_into(layer.subagent.clone());
        }

        let max_steps = global
            .as_ref()
            .and_then(|s| s.runtime.max_steps)
            .unwrap_or(64);
        // `max_steps == 0` is allowed and means "no execution cap" (see `RuntimeSettings`).
        Self::finalize(merged, compression, memory, subagent, max_steps)
    }

    /// Convenience entry point: uses the real `config_dir` and process env vars.
    pub fn load_default(workspace_root: &Path) -> Result<Self, SettingsError> {
        let paths =
            crate::storage::StoragePaths::new(crate::storage::global_root(), workspace_root)?;
        paths.migrate()?;
        Self::load(Some(&crate::storage::config_path()), &|key| {
            std::env::var(key).ok()
        })
    }

    /// Whether model service configuration is complete (i.e. `base_url` is non-empty).
    ///
    /// Missing config is not an error and does not block startup: the application layer uses this to decide whether to automatically enter
    /// `/login` in the TUI to guide the user to fill it in. With an empty `base_url` model requests fail, so when unconfigured
    /// "it starts" should not be mistaken for "it works".
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty()
    }

    /// Settle the merge result into the final config, filling in defaults.
    ///
    /// A missing `base_url` **no longer errors**: that would block startup. In that case `base_url` is an empty string,
    /// [`Settings::is_configured`] returns `false`, and the application layer uses this to guide the user
    /// through `/login` in the TUI (see `main.rs`).
    fn finalize(
        merged: ProviderSettings,
        compression: CompressionSettings,
        memory: MemorySettings,
        subagent: SubagentSettings,
        max_steps: usize,
    ) -> Result<Self, SettingsError> {
        let base_url = merged
            .base_url
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_default();

        let protocol = match &merged.protocol {
            Some(name) => parse_protocol(name)?,
            None => ModelProtocol::ChatCompletions,
        };

        let context_window_tokens = match merged.context_window_tokens {
            Some(0) => {
                return Err(SettingsError::Invalid(
                    "context_window_tokens must be greater than 0".into(),
                ));
            }
            Some(n) => n,
            None => DEFAULT_CONTEXT_WINDOW_TOKENS,
        };

        let model = merged
            .model
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());

        // Image input is optimistic by default (`docs/multimodal.md`): assume the model can
        // read images and let the adapter strip + resend silently if it really cannot. The
        // `[provider] supports_images` / `LOCAL_SUPPORTS_IMAGES` switch is an internal escape
        // hatch, not something a normal user ever sets.
        let supports_images = merged.supports_images.unwrap_or(true);

        // The thinking switch defaults to on; reasoning effort defaults to `low` (consistent with the old hardcoding).
        let thinking = merged.thinking.unwrap_or(true);
        let reasoning_effort = merged
            .reasoning_effort
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_REASONING_EFFORT.to_owned());
        // TOML escape hatch -> JSON (the SDK's `extra_body` is `serde_json::Value`).
        let extra_body = match merged.extra_body {
            Some(value) => Some(serde_json::to_value(value).map_err(|error| {
                SettingsError::Invalid(format!("extra_body cannot be converted to JSON: {error}"))
            })?),
            None => None,
        };

        // Tool transport mode: only accepts `native` / `content` (case-insensitive); any other value is a config error.
        let tool_mode = match merged.tool_mode {
            Some(mode) => {
                let normalized = mode.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    "native" | "content" => Some(normalized),
                    other => {
                        return Err(SettingsError::Invalid(format!(
                            "tool_mode can only be native or content, got: {other}"
                        )));
                    }
                }
            }
            None => None,
        };

        Ok(Self {
            protocol,
            base_url,
            api_key: merged.api_key.filter(|v| !v.is_empty()),
            models_url: merged.models_url.filter(|v| !v.trim().is_empty()),
            model,
            context_window_tokens,
            supports_images,
            thinking,
            reasoning_effort,
            extra_body,
            tool_mode,
            compression,
            memory,
            subagent,
            max_steps,
        })
    }
}

/// Tighten file permissions to owner read/write only (`0600`). Skipped on non-Unix platforms.
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Write a set of provider config into the config file (**read first, modify, then write**; only overrides the passed-in fields,
/// leaving other keys in the file untouched). Returns the written path for the caller to report to the user.
///
/// Persistence entry point for `/login`. Production callers pass the shared global
/// config path; tests can inject an isolated temporary path.
pub fn save_provider(path: &Path, provider: ProviderSettings) -> Result<PathBuf, SettingsError> {
    let mut existing = FileSettings::load_file(path)?.unwrap_or_default();
    merge_into(&mut existing.provider, provider);
    existing.save(path)?;
    Ok(path.to_path_buf())
}

/// The env var layer: maps the process environment into a [`ProviderSettings`].
fn env_layer(env: &dyn Fn(&str) -> Option<String>) -> ProviderSettings {
    ProviderSettings {
        protocol: env("LOCAL_PROTOCOL"),
        base_url: env("LOCAL_BASE_URL"),
        api_key: env("LOCAL_API_KEY"),
        models_url: env("LOCAL_MODELS_URL"),
        model: env("LOCAL_MODEL"),
        context_window_tokens: env("LOCAL_CONTEXT_WINDOW_TOKENS").and_then(|v| v.parse().ok()),
        supports_images: env("LOCAL_SUPPORTS_IMAGES").map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        }),
        // `LOCAL_THINKING` is parsed with the usual truthy spellings; when absent it makes no statement (None), yielding to the file layer / built-in defaults.
        thinking: env("LOCAL_THINKING").map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        }),
        reasoning_effort: env("LOCAL_REASONING_EFFORT"),
        // The escape hatch structure is complex; it only goes through the config file, no env var is provided.
        extra_body: None,
        tool_mode: env("LOCAL_TOOL_MODE"),
    }
}

/// Layer-by-layer override: a higher-priority `Some` beats a lower-priority `Some`.
///
/// Inputs are ordered **from lowest to highest priority**; `None` layers are skipped directly.
fn merge_all(layers: [Option<ProviderSettings>; 2]) -> ProviderSettings {
    let mut merged = ProviderSettings::default();
    for layer in layers.into_iter().flatten() {
        merge_into(&mut merged, layer);
    }
    merged
}

fn merge_into(base: &mut ProviderSettings, over: ProviderSettings) {
    macro_rules! take {
        ($($field:ident),* $(,)?) => {
            $(if over.$field.is_some() { base.$field = over.$field; })*
        };
    }
    take!(
        protocol,
        base_url,
        api_key,
        models_url,
        model,
        context_window_tokens,
        supports_images,
        thinking,
        reasoning_effort,
        extra_body,
        tool_mode,
    );
}

/// Public entry point for protocol-name parsing (for `bootstrap` to parse `[memory] curator_protocol`).
pub fn parse_protocol_name(name: &str) -> Result<ModelProtocol, SettingsError> {
    parse_protocol(name)
}

/// Protocol-name parsing. Accepts common aliases; an unknown name errors (no silent fallback).
fn parse_protocol(name: &str) -> Result<ModelProtocol, SettingsError> {
    let normalized = name.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "chat_completions" | "chatcompletions" | "openai" => Ok(ModelProtocol::ChatCompletions),
        "responses" => Ok(ModelProtocol::Responses),
        "anthropic_messages" | "anthropic" | "messages" => Ok(ModelProtocol::AnthropicMessages),
        other => Err(SettingsError::Invalid(format!(
            "unknown protocol `{other}`, options: chat_completions / responses / anthropic_messages"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shirley_settings_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn env_supplies_base_url_and_defaults_fill_rest() {
        let root = temp_root("env_only");
        let settings = Settings::load(
            Some(&root.join("config.toml")),
            &env_of(&[("LOCAL_BASE_URL", "http://x/v1/chat/completions")]),
        )
        .unwrap();
        assert_eq!(settings.base_url, "http://x/v1/chat/completions");
        assert_eq!(settings.protocol, ModelProtocol::ChatCompletions);
        assert_eq!(settings.model, DEFAULT_MODEL);
        assert_eq!(
            settings.context_window_tokens,
            DEFAULT_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(settings.api_key, None);
        assert_eq!(settings.max_steps, 64);
    }

    #[test]
    fn runtime_limit_is_global_and_allows_zero_as_unlimited() {
        let root = temp_root("runtime_budget");
        let global = root.join("global.toml");
        std::fs::write(&global, "[runtime]\nmax_steps = 128\n").unwrap();
        let ws = root.clone();
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("config.toml"),
            "[compression]\nretain_ratio = 0.3\n",
        )
        .unwrap();
        assert_eq!(
            Settings::load(Some(&global), &env_of(&[]))
                .unwrap()
                .max_steps,
            128
        );
        std::fs::write(&global, "[runtime]\nmax_steps = 12\n").unwrap();
        assert_eq!(
            Settings::load(Some(&global), &env_of(&[]))
                .unwrap()
                .max_steps,
            12
        );
        // `0` is the documented "unlimited" sentinel and must be accepted (not an error).
        std::fs::write(&global, "[runtime]\nmax_steps = 0\n").unwrap();
        assert_eq!(
            Settings::load(Some(&global), &env_of(&[]))
                .unwrap()
                .max_steps,
            0
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_base_url_yields_unconfigured_not_error() {
        // A missing base_url is no longer an error (does not block startup), but an "unconfigured" state.
        let root = temp_root("missing");
        let settings = Settings::load(Some(&root.join("config.toml")), &env_of(&[])).unwrap();
        assert_eq!(settings.base_url, "");
        assert!(!settings.is_configured());
        // The remaining fields still fill in defaults, so ModelConfig can still be constructed.
        assert_eq!(settings.model, DEFAULT_MODEL);
    }

    #[test]
    fn present_base_url_is_configured() {
        let root = temp_root("configured");
        let settings = Settings::load(
            Some(&root.join("config.toml")),
            &env_of(&[("LOCAL_BASE_URL", "http://x")]),
        )
        .unwrap();
        assert!(settings.is_configured());
    }

    #[test]
    fn only_global_file_supplies_settings() {
        let root = temp_root("ws_over_global");
        let global_dir = temp_root("global_dir");
        let global = global_dir.join("config.toml");
        std::fs::write(
            &global,
            "[provider]\nbase_url = \"http://global\"\nmodel = \"g-model\"\n",
        )
        .unwrap();
        let ws_dir = root.clone();
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[provider]\nbase_url = \"http://workspace\"\n",
        )
        .unwrap();

        let settings = Settings::load(Some(&global), &env_of(&[])).unwrap();
        // Only the global file is read; the unrelated file cannot override it.
        assert_eq!(settings.base_url, "http://global");
        assert_eq!(settings.model, "g-model");
    }

    #[test]
    fn env_overrides_files() {
        let root = temp_root("env_over_file");
        let ws_dir = root.clone();
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[provider]\nbase_url = \"http://file\"\napi_key = \"file-key\"\n",
        )
        .unwrap();

        let settings = Settings::load(
            Some(&root.join("config.toml")),
            &env_of(&[
                ("LOCAL_BASE_URL", "http://env"),
                ("LOCAL_MODEL", "env-model"),
            ]),
        )
        .unwrap();
        assert_eq!(
            settings.base_url, "http://env",
            "env should override the file"
        );
        assert_eq!(settings.model, "env-model");
        // Keys present in the file but not in env should be retained.
        assert_eq!(settings.api_key.as_deref(), Some("file-key"));
    }

    #[test]
    fn parses_provider_table_from_file() {
        let root = temp_root("parse_table");
        let ws_dir = root.clone();
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[provider]\nprotocol = \"responses\"\nbase_url = \"http://x\"\ncontext_window_tokens = 128000\n",
        )
        .unwrap();
        let settings = Settings::load(Some(&root.join("config.toml")), &env_of(&[])).unwrap();
        assert_eq!(settings.protocol, ModelProtocol::Responses);
        assert_eq!(settings.context_window_tokens, 128_000);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let root = temp_root("unknown_key");
        let ws_dir = root.clone();
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[provider]\nbase_url = \"http://x\"\nbas_url = \"typo\"\n",
        )
        .unwrap();
        let err = Settings::load(Some(&root.join("config.toml")), &env_of(&[])).unwrap_err();
        assert!(
            matches!(err, SettingsError::Parse { .. }),
            "a typo'd key should error: {err}"
        );
    }

    #[test]
    fn zero_context_window_is_rejected() {
        let root = temp_root("zero_ctx");
        let err = Settings::load(
            Some(&root.join("config.toml")),
            &env_of(&[
                ("LOCAL_BASE_URL", "http://x"),
                ("LOCAL_CONTEXT_WINDOW_TOKENS", "0"),
            ]),
        )
        .unwrap_err();
        assert!(matches!(err, SettingsError::Invalid(_)));
    }

    #[test]
    fn unknown_protocol_is_rejected() {
        let root = temp_root("bad_proto");
        let err = Settings::load(
            Some(&root.join("config.toml")),
            &env_of(&[("LOCAL_BASE_URL", "http://x"), ("LOCAL_PROTOCOL", "gemini")]),
        )
        .unwrap_err();
        assert!(matches!(err, SettingsError::Invalid(_)));
    }

    #[test]
    fn save_provider_preserves_other_keys() {
        let root = temp_root("save_preserve");
        let path = root.join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Pre-set a models_url (not overridden this time).
        std::fs::write(
            &path,
            "[provider]\nmodels_url = \"http://keep/v1/models\"\nbase_url = \"http://old\"\n",
        )
        .unwrap();

        save_provider(
            &path,
            ProviderSettings {
                base_url: Some("http://new".into()),
                api_key: Some("sk".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let saved = FileSettings::load_file(&path).unwrap().unwrap();
        assert_eq!(saved.provider.base_url.as_deref(), Some("http://new"));
        assert_eq!(saved.provider.api_key.as_deref(), Some("sk"));
        assert_eq!(
            saved.provider.models_url.as_deref(),
            Some("http://keep/v1/models"),
            "keys not overridden should be retained"
        );
    }

    #[test]
    fn save_provider_creates_missing_dirs() {
        let root = temp_root("save_dirs");
        let path = root.join("nested/deep/config.toml");
        save_provider(
            &path,
            ProviderSettings {
                base_url: Some("http://x".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(path.exists(), "should auto-create parent directories");
    }

    #[test]
    fn saved_provider_round_trips_through_load() {
        let root = temp_root("save_roundtrip");
        let path = root.join("config.toml");
        save_provider(
            &path,
            ProviderSettings {
                base_url: Some("http://saved".into()),
                model: Some("m".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let settings = Settings::load(Some(&root.join("config.toml")), &env_of(&[])).unwrap();
        assert_eq!(settings.base_url, "http://saved");
        assert_eq!(settings.model, "m");
    }

    #[test]
    fn empty_file_is_empty_layer() {
        let root = temp_root("empty_file");
        let ws_dir = root.clone();
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(ws_dir.join("config.toml"), "   \n").unwrap();
        let settings = Settings::load(
            Some(&root.join("config.toml")),
            &env_of(&[("LOCAL_BASE_URL", "http://x")]),
        )
        .unwrap();
        assert_eq!(settings.base_url, "http://x");
    }

    #[test]
    fn memory_settings_come_from_global_file() {
        let root = temp_root("memory_table");
        let global_dir = temp_root("memory_global");
        let global = global_dir.join("config.toml");
        std::fs::write(
            &global,
            "[memory]\ncurator_model = \"g-curator\"\nconsolidate_after_entries = 10\n",
        )
        .unwrap();
        let ws_dir = root.clone();
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[memory]\ncurator_model = \"ws-curator\"\ncurator_base_url = \"http://c\"\n",
        )
        .unwrap();

        let settings = Settings::load(Some(&global), &env_of(&[])).unwrap();
        // All memory settings come from the global file.
        assert_eq!(settings.memory.curator_model.as_deref(), Some("g-curator"));
        assert_eq!(settings.memory.curator_base_url, None);
        assert_eq!(settings.memory.consolidate_after_entries, Some(10));
    }

    #[test]
    fn global_embedding_is_inherited_by_chat_and_project_workspaces() {
        let root = temp_root("global_embedding_inheritance");
        let global = root.join("global.toml");
        std::fs::write(
            &global,
            "[memory]\nembedding_base_url = \"http://localhost:11434/v1/embeddings\"\nembedding_model = \"nomic-embed-text\"\n",
        )
        .unwrap();
        let chat = root.join("chat");
        let project = root.join("project");
        std::fs::create_dir_all(&chat).unwrap();
        std::fs::create_dir_all(project.join(".shirley")).unwrap();
        // Project-specific curator settings must not erase global embedding settings.
        std::fs::write(
            project.join(".shirley/config.toml"),
            "[memory]\ncurator_model = \"project-curator\"\n",
        )
        .unwrap();
        for _workspace in [&chat, &project] {
            let settings = Settings::load(Some(&global), &env_of(&[])).unwrap();
            assert_eq!(
                settings.memory.embedding_base_url.as_deref(),
                Some("http://localhost:11434/v1/embeddings")
            );
            assert_eq!(
                settings.memory.embedding_model.as_deref(),
                Some("nomic-embed-text")
            );
            assert_eq!(settings.memory.embedding_api_key, None);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn memory_table_absent_is_default() {
        let root = temp_root("memory_absent");
        let settings = Settings::load(
            Some(&root.join("config.toml")),
            &env_of(&[("LOCAL_BASE_URL", "http://x")]),
        )
        .unwrap();
        assert_eq!(settings.memory, MemorySettings::default());
    }

    #[test]
    fn thinking_defaults_to_true_and_effort_to_low() {
        let root = temp_root("thinking_default");
        let settings = Settings::load(
            Some(&root.join("config.toml")),
            &env_of(&[("LOCAL_BASE_URL", "http://x")]),
        )
        .unwrap();
        assert!(settings.thinking, "default should enable thinking");
        assert_eq!(settings.reasoning_effort, "low");
        assert_eq!(settings.extra_body, None);
    }

    #[test]
    fn thinking_and_extra_body_parse_from_provider_table() {
        let root = temp_root("thinking_extra");
        let ws_dir = root.clone();
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[provider]\nbase_url = \"http://x\"\nthinking = false\nreasoning_effort = \"high\"\n\n[provider.extra_body]\nbudget_tokens = 2048\n",
        )
        .unwrap();

        let settings = Settings::load(Some(&root.join("config.toml")), &env_of(&[])).unwrap();
        assert!(!settings.thinking, "thinking should be configured to false");
        assert_eq!(settings.reasoning_effort, "high");
        assert_eq!(
            settings.extra_body,
            Some(serde_json::json!({ "budget_tokens": 2048 }))
        );
    }
}
