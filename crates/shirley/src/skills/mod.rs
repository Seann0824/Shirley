//! Skills: pluggable capability packs following the Agent Skills open standard (**an
//! application-layer capability**).
//!
//! See `docs/skills.md`. A skill is a directory that minimally contains `SKILL.md` (YAML
//! frontmatter + Markdown body) and may carry `scripts/` / `references/` / `assets/`.
//!
//! It lives in the app layer for the same reason as `todo.rs` / `memory/`: which directories to
//! discover, how to disclose, and how to activate are trade-offs of "the coding-agent product".
//! The SDK provides only the generic [`ContextProvider`] seam; this module adds **no new SDK
//! public types**.
//!
//! **Progressive disclosure (`docs/skills.md` 2.2)**:
//! - **Tier1** ([`catalog`]): the directory is always present — one line per skill with
//!   `name + description + location`;
//! - **Tier2** ([`runtime`] + [`tool`]): `activate_skill(name)` activates, and the body is
//!   **appended at the end of each turn** by [`provider`] (survives compaction);
//! - **Tier3** ([`tool`]): `read_skill_file(skill, path)` reads a resource on demand.
//!
//! **KV prefix cache**: injection is always **append-only** — the directory is a stable prefix,
//! activated blocks are concatenated after it in activation order, and old content stays
//! byte-for-byte unchanged (see [`runtime::SkillRuntime::render`]).
//!
//! [`ContextProvider`]: shirley_agent_sdk::ContextProvider

pub mod catalog;
pub mod discovery;
pub mod parse;
pub mod provider;
pub mod runtime;

pub use catalog::SkillCatalog;
pub use provider::SkillContextProvider;
pub use runtime::SkillRuntime;

// Public surface of discovery / parsing (used by the existing P0 wiring and tests).
#[allow(
    unused_imports,
    reason = "public re-export used by tests and downstream modules"
)]
pub use discovery::Discovery;
#[allow(
    unused_imports,
    reason = "public re-export used by tests and downstream modules"
)]
pub use parse::{ParseError, SkillMeta, parse_skill};

/// User-level root for discovering skills (the parent of `~/.agents/skills` and
/// `~/.claude/skills`).
///
/// Returns `None` when the user home is unavailable (project level only).
pub fn user_home() -> Option<std::path::PathBuf> {
    dirs::home_dir()
}
