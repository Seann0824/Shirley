//! 技能配套工具：`activate_skill`（Tier2 激活）与 `read_skill_file`（Tier3 资源）。
//!
//! 两者都是**手写 `Tool`**（与 `TodoTool` / `RecallTool` 同款），不是 `#[tool]` 宏——
//! 因为它们的参数 schema 需要把 `name` / `skill` 限制成**枚举**（防模型幻觉出一个
//! 不存在的 skill 名，`docs/skills.md` 决策 3、六）。宏从签名生成的 schema 表达不了
//! 这种运行期才确定的枚举，故手写。
//!
//! 状态由 `SkillRuntime`（`Arc` 共享）承载，与 `SkillContextProvider` 是同一份——
//! `activate_skill` 写、`provider` 读，与 todo 的 `TodoStore` 完全同构。

use std::sync::Arc;

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

use super::catalog::SkillCatalog;
use super::runtime::{ActivateError, SkillRuntime};
use crate::tools::read::read_text_range;

/// `activate_skill(name)`：读一个 skill 的正文（剥 frontmatter）并标记为已激活。
///
/// 激活后，该 skill 的正文会由 `SkillContextProvider` **每轮追加注入**（跨压缩
/// 存活）——所以工具返回值是给模型**本轮**立即看指令用的，不依赖它记住。
pub struct ActivateSkillTool {
    runtime: Arc<SkillRuntime>,
    definition: ToolDefinition,
}

impl ActivateSkillTool {
    pub fn new(runtime: Arc<SkillRuntime>) -> Self {
        // name 限制为目录里真实存在的 skill（枚举），防幻觉。
        let names = runtime.skill_names();
        let definition = ToolDefinition {
            name: "activate_skill".into(),
            description: "Load a skill's full instructions before starting a task it covers. \
                Call this when a task matches a skill listed in <available_skills>. After \
                activation the instructions stay available in every turn (they survive context \
                compaction). Read the skill's resources on demand with `read_skill_file`."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "enum": names,
                        "description": "Name of the skill to activate (must be one of the available skills)."
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        };
        Self { runtime, definition }
    }
}

impl Tool for ActivateSkillTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            let name = input
                .get("name")
                .and_then(|value| value.as_str())
                .ok_or_else(|| ToolError::ArgumentsError("missing `name` argument".into()))?;

            match runtime.activate(name) {
                Ok(block) => Ok(serde_json::Value::String(format!(
                    "skill `{name}` activated. Its instructions are now injected every turn.\n\n{block}"
                ))),
                Err(ActivateError::UnknownSkill(name)) => Err(ToolError::ArgumentsError(format!(
                    "unknown skill `{name}`; use one of the skills listed in <available_skills>"
                ))),
                Err(ActivateError::ReadFailed(reason)) => {
                    Err(ToolError::ExecutionError(format!("failed to load skill: {reason}")))
                }
            }
        })
    }
}

/// `read_skill_file(skill, path)`：按需读取 skill 目录里的资源（Tier3）。
///
/// 用户级 skill 在工作区外，`read_file` 够不到——本工具把根**钉死**在 `skill.root`，
/// 复用 `read_file` 的读取内核（同一套行/字节上限、密钥过滤、二进制探测、越界校验），
/// 只是换了个根（`docs/skills.md` 5.5）。
pub struct ReadSkillFileTool {
    catalog: Arc<SkillCatalog>,
    definition: ToolDefinition,
}

impl ReadSkillFileTool {
    pub fn new(catalog: Arc<SkillCatalog>) -> Self {
        let names = catalog.skill_names();
        let definition = ToolDefinition {
            name: "read_skill_file".into(),
            description: "Read a file from a skill's directory (e.g. a script, reference, or \
                template) on demand. Use this for resources listed under <resources> in an \
                activated skill. Path is relative to the skill root; `..` and absolute paths \
                are rejected."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "skill": {
                        "type": "string",
                        "enum": names,
                        "description": "Name of the skill that owns the file."
                    },
                    "path": {
                        "type": "string",
                        "description": "Path relative to the skill root, e.g. `scripts/run.py`."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "First line to read (1-based). Omit for line 1."
                    },
                    "line_count": {
                        "type": "integer",
                        "description": "Max lines to read. Default 200, max 2000."
                    }
                },
                "required": ["skill", "path"],
                "additionalProperties": false
            }),
        };
        Self { catalog, definition }
    }
}

impl Tool for ReadSkillFileTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        let catalog = self.catalog.clone();
        Box::pin(async move {
            let skill = input
                .get("skill")
                .and_then(|value| value.as_str())
                .ok_or_else(|| ToolError::ArgumentsError("missing `skill` argument".into()))?;
            let path = input
                .get("path")
                .and_then(|value| value.as_str())
                .ok_or_else(|| ToolError::ArgumentsError("missing `path` argument".into()))?;
            let start_line = input
                .get("start_line")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize);
            let line_count = input
                .get("line_count")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize);

            let meta = catalog.get(skill).ok_or_else(|| {
                ToolError::ArgumentsError(format!(
                    "unknown skill `{skill}`; use one of the skills listed in <available_skills>"
                ))
            })?;

            // display = `skill/path`，让模型看到来源；读取内核仍以 skill.root 为根校验。
            let display = format!("{skill}/{path}");
            read_text_range(&meta.root, path, &display, start_line, line_count)
                .await
                .map(serde_json::Value::String)
        })
    }
}
