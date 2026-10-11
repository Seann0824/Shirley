//! 技能（skills）：拥抱 Agent Skills 开放标准的可插拔能力包（**应用层能力**）。
//!
//! 设计见 `docs/skills.md`。一个 skill 是一个目录，最小含 `SKILL.md`（YAML
//! frontmatter + Markdown 正文），可携带 `scripts/` / `references/` / `assets/`。
//!
//! 归应用层的原因与 `todo.rs` / `memory/` 一致：发现哪些目录、怎么披露、怎么激活
//! 是"coding agent 这个产品"的取舍。SDK 只提供通用接缝 [`ContextProvider`]，
//! 本模块**不新增任何 SDK 对外类型**。
//!
//! **渐进式披露（`docs/skills.md` 2.2）**：
//! - **Tier1**（[`catalog`]）：目录常驻——每 skill 一行 `name + description + location`；
//! - **Tier2**（[`runtime`] + [`tool`]）：`activate_skill(name)` 激活，正文由
//!   [`provider`] **每轮末尾追加注入**（跨压缩存活）；
//! - **Tier3**（[`tool`]）：`read_skill_file(skill, path)` 按需读资源。
//!
//! **KV 前缀缓存**：注入恒为**追加式**——目录是稳定前缀，已激活块按激活顺序拼接在
//! 其后，旧内容逐字节不变（见 [`runtime::SkillRuntime::render`]）。
//!
//! [`ContextProvider`]: shirley_agent_sdk::ContextProvider

pub mod catalog;
pub mod discovery;
pub mod parse;
pub mod provider;
pub mod runtime;
pub mod tool;

pub use catalog::SkillCatalog;
pub use provider::SkillContextProvider;
pub use runtime::SkillRuntime;
pub use tool::{ActivateSkillTool, ReadSkillFileTool};

// 发现 / 解析的公开面（供 P0 已有接线与测试使用）。
#[allow(unused_imports)]
pub use discovery::{discover, Discovery};
#[allow(unused_imports)]
pub use parse::{parse_skill, ParseError, SkillMeta};

/// 发现 skill 的用户级根目录（`~/.agents/skills` 与 `~/.claude/skills` 的父目录）。
///
/// 拿不到用户 home 时返回 `None`（只扫项目级）。
pub fn user_home() -> Option<std::path::PathBuf> {
    dirs::home_dir()
}
