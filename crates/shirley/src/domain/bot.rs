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
    /// Skills (`SKILL.md` packages from the user-level skill roots) this bot may activate.
    /// Empty = every discovered skill; named = exactly these.
    #[serde(default)]
    pub skills: Vec<String>,
    /// The bot's identity block, rendered at the top of the system prompt in place of the
    /// platform default. `None` = the generic Shirley identity.
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
            skills: Vec::new(),
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
                skills: Vec::new(),
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
                skills: Vec::new(),
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
                skills: Vec::new(),
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

    /// A valid bot name: 1-32 chars of `a-z`, `0-9`, `-`, starting with a letter. It is a
    /// directory name and a URL segment, so nothing else is accepted.
    pub fn is_valid_name(name: &str) -> bool {
        let mut chars = name.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
            && name.len() <= 32
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    /// Write this spec to `<bots_root>/<name>/spec.toml`, creating the bot's directory.
    pub fn save(&self, bots_root: &Path) -> Result<(), String> {
        if !Self::is_valid_name(&self.name) {
            return Err(format!(
                "invalid bot name `{}`: use 1-32 lowercase letters, digits or `-`, starting with a letter",
                self.name
            ));
        }
        let dir = bots_root.join(&self.name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("spec.toml"), text).map_err(|e| e.to_string())
    }
}

impl Default for BotSpec {
    fn default() -> Self {
        Self::default_bot()
    }
}

impl BotSpec {
    /// The capability tools this bot actually gets: its own list, or the platform default set.
    pub fn effective_tools(&self) -> Vec<String> {
        if self.has_tool_restriction() {
            self.tools.clone()
        } else {
            platform_tools::DEFAULT_SET
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        }
    }
}

/// Names of the platform's **capability** tools -- the part of the table a bot may opt out of.
/// Infrastructure tools (todo / recall / skills / computer) are always registered.
pub mod platform_tools {
    pub const BASH: &str = "bash";
    pub const GREP: &str = "grep";
    pub const READ: &str = "read";
    pub const MEDIA: &str = "media";
    pub const WEB_SEARCH: &str = "web_search";

    /// The default capability set every bot receives unless it overrides `tools`.
    pub const DEFAULT_SET: &[&str] = &[BASH, GREP, READ, MEDIA, WEB_SEARCH];
}
