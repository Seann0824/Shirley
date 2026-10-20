//! Skill companion tools: `activate_skill` (Tier2 activation) and `read_skill_file` (Tier3
//! resources).
//!
//! Both are **hand-written `Tool`s** (same as `TodoTool` / `RecallTool`), not `#[tool]` macros —
//! because their argument schema needs to constrain `name` / `skill` to an **enum** (to stop the
//! model from hallucinating a nonexistent skill name; `docs/skills.md` decisions 3 and 6). A
//! schema generated from a signature can't express an enum only known at runtime, hence the
//! hand-written form.
//!
//! State is carried by `SkillRuntime` (shared via `Arc`), the same instance as
//! `SkillContextProvider` — `activate_skill` writes and `provider` reads, exactly isomorphic to
//! todo's `TodoStore`.

use std::sync::Arc;

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

use crate::skills::catalog::SkillCatalog;
use crate::skills::runtime::{ActivateError, SkillRuntime};
use crate::tools::read::read_text_range;

/// `activate_skill(name)`: read a skill's body (stripping frontmatter) and mark it activated.
///
/// After activation, the skill's body is **appended each turn** by `SkillContextProvider`
/// (survives compaction) — so the tool's return value is for the model to read the instructions
/// **immediately this turn**, not something it must remember.
pub struct ActivateSkillTool {
    runtime: Arc<SkillRuntime>,
    definition: ToolDefinition,
}

impl ActivateSkillTool {
    pub fn new(runtime: Arc<SkillRuntime>) -> Self {
        // Constrain `name` to skills that really exist in the catalog (an enum), to prevent hallucination.
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
        Self {
            runtime,
            definition,
        }
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
                Err(ActivateError::ReadFailed(reason)) => Err(ToolError::ExecutionError(format!(
                    "failed to load skill: {reason}"
                ))),
            }
        })
    }
}

/// `read_skill_file(skill, path)`: read a resource inside a skill directory on demand (Tier3).
///
/// User-level skills live outside the workspace, so `read` can't reach them — this tool
/// **pins** the root to `skill.root` and reuses `read`'s reading kernel (the same line/byte
/// limits, secret filtering, binary detection, and out-of-bounds checks), just with a different
/// root (`docs/skills.md` 5.5).
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
        Self {
            catalog,
            definition,
        }
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

            // display = `skill/path` so the model sees the source; the reading kernel still validates against skill.root.
            let display = format!("{skill}/{path}");
            read_text_range(&meta.root, path, &display, start_line, line_count)
                .await
                .map(serde_json::Value::String)
        })
    }
}
