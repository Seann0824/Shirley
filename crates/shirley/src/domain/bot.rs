//! Bot specification: the declaration of a bot's identity and capabilities.
//!
//! A `BotSpec` names the bot and declares which tools from the platform catalog it uses.
//! The platform catalog (`crate::tools`) registers all available tools; each bot references
//! tools by name. If `tools` is empty the bot gets the full default tool set.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Declaration of a bot's identity and tool set.
///
/// Bots are defined in `~/.shirley/bots/<name>/spec.toml` or seeded by the platform defaults.
/// The `tools` list names tools from the platform catalog; an empty list means "all defaults".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotSpec {
    /// Unique bot identifier (URL-safe slug, e.g. `"nono"`, `"patch"`, `"scout"`).
    pub name: String,
    /// Human-readable display name (e.g. `"NONO"`, `"Patch"`).
    pub display_name: String,
    /// Tools this bot is allowed to use.
    /// Empty = use the platform default set (all tools).
    /// Named = restrict to exactly these tool names.
    #[serde(default)]
    pub tools: Vec<String>,
    /// System prompt override appended after the base platform prompt.
    /// `None` = use the base prompt unchanged.
    #[serde(default)]
    pub system_prompt_extra: Option<String>,
}

impl BotSpec {
    /// The default platform bot: all tools, no specialization.
    pub fn default_bot() -> Self {
        Self {
            name: "default".to_owned(),
            display_name: "Shirley".to_owned(),
            tools: Vec::new(),
            system_prompt_extra: None,
        }
    }

    /// Built-in platform bots seeded when no user-defined spec exists.
    pub fn platform_defaults() -> Vec<Self> {
        vec![
            Self {
                name: "nono".to_owned(),
                display_name: "NONO".to_owned(),
                tools: Vec::new(),
                system_prompt_extra: None,
            },
            Self {
                name: "patch".to_owned(),
                display_name: "Patch".to_owned(),
                tools: vec![
                    platform_tools::BASH.to_owned(),
                    platform_tools::GREP.to_owned(),
                    platform_tools::READ.to_owned(),
                ],
                system_prompt_extra: Some(
                    "You are Patch, a focused coding assistant. You help with code changes, reviews, and debugging.".to_owned(),
                ),
            },
            Self {
                name: "scout".to_owned(),
                display_name: "Scout".to_owned(),
                tools: vec![
                    platform_tools::WEB_SEARCH.to_owned(),
                    platform_tools::READ.to_owned(),
                ],
                system_prompt_extra: Some(
                    "You are Scout, a research assistant. You search the web and summarize information.".to_owned(),
                ),
            },
        ]
    }

    /// Load a spec from `<bots_root>/<name>/spec.toml`, or fall back to a platform default if the
    /// bot name matches one of the built-in bots, or finally return the generic default bot.
    pub fn load_or_default(name: &str, bots_root: &Path) -> Self {
        let spec_path = bots_root.join(name).join("spec.toml");
        if let Ok(text) = std::fs::read_to_string(&spec_path) {
            if let Ok(spec) = toml::from_str::<Self>(&text) {
                return spec;
            }
        }
        // Fall back to a named platform default.
        Self::platform_defaults()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| Self {
                name: name.to_owned(),
                display_name: name.to_owned(),
                ..Self::default_bot()
            })
    }

    /// List all bots: user-defined ones under `<bots_root>/` plus any platform defaults not
    /// overridden by a user file.
    pub fn list_all(bots_root: &Path) -> Vec<Self> {
        let mut specs: Vec<Self> = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        // User-defined bots first.
        if let Ok(entries) = std::fs::read_dir(bots_root) {
            for entry in entries.flatten() {
                let spec_path = entry.path().join("spec.toml");
                if let Ok(text) = std::fs::read_to_string(&spec_path) {
                    if let Ok(spec) = toml::from_str::<Self>(&text) {
                        seen_names.insert(spec.name.clone());
                        specs.push(spec);
                    }
                }
            }
        }

        // Then platform defaults not already covered.
        for default in Self::platform_defaults() {
            if !seen_names.contains(&default.name) {
                specs.push(default);
            }
        }

        specs
    }

    /// Whether the spec restricts tools, or accepts the full default set.
    pub fn has_tool_restriction(&self) -> bool {
        !self.tools.is_empty()
    }
}

impl Default for BotSpec {
    fn default() -> Self {
        Self::default_bot()
    }
}

/// Names of the platform's built-in tools, available to any bot.
pub mod platform_tools {
    pub const BASH: &str = "bash";
    pub const GREP: &str = "grep";
    pub const READ: &str = "read";
    pub const MEDIA: &str = "media";
    pub const WEB_SEARCH: &str = "web_search";
    pub const TODO: &str = "todo";
    pub const RECALL: &str = "recall";
    pub const ACTIVATE_SKILL: &str = "activate_skill";
    pub const READ_SKILL_FILE: &str = "read_skill_file";
    pub const COMPUTER_APPS: &str = "computer_apps";
    pub const COMPUTER_SNAPSHOT: &str = "computer_snapshot";
    pub const COMPUTER_ACT: &str = "computer_act";
    pub const COMPUTER_KEY: &str = "computer_key";

    /// The default tool set every bot receives unless it overrides `tools`.
    pub const DEFAULT_SET: &[&str] = &[
        BASH, GREP, READ, MEDIA, WEB_SEARCH, TODO, RECALL, ACTIVATE_SKILL, READ_SKILL_FILE,
        COMPUTER_APPS, COMPUTER_SNAPSHOT, COMPUTER_ACT, COMPUTER_KEY,
    ];
}
