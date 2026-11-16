//! Bot specification: the declaration of a bot's identity and capabilities.
//!
//! A `BotSpec` names the bot and declares which tools from the platform catalog it uses.
//! The platform catalog (`crate::tools`) registers all available tools; each bot references
//! tools by name. If `tools` is empty the bot gets the full default tool set.

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
