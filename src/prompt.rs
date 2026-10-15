//! Application-side system prompt construction.
//!
//! The SDK only provides the mechanism that "the prompt can be a function and receives the runtime context"
//! (`shirley_agent_sdk::SystemPrompt` / `SystemPromptContext`). **What content to assemble** is a
//! business decision, and it lives here.
//!
//! **Text separated from code**: the prompt body is maintained in `prompts/*.md` (embedded at compile time via `include_str!`);
//! the code only "picks a template + fills placeholders". It currently assembles three blocks:
//!
//! 1. Role and work boundary (fixed text in `prompts/system.md`).
//! 2. The current working directory -- a pain point noted in `plan.md`: if the AI does not know the workspace it will start
//!    searching randomly from the filesystem root, wasting tokens. Telling it the root explicitly narrows the search scope.
//! 3. The project guide file (`Agent.md`) -- tells the AI "how this project runs and how the code is organized".

use std::path::{Path, PathBuf};

use shirley_agent_sdk::{SystemPrompt, SystemPromptContext};

use crate::prompt_template::render as render_template;

/// Project guide file name. Placed at the workspace root as the Agent's working reference.
pub const GUIDE_FILE_NAME: &str = "Agent.md";

/// System prompt template (role / work boundary / workspace / project guide).
const SYSTEM_TEMPLATE: &str = include_str!("../prompts/system.md");
/// Inline template for when the project guide is present.
const GUIDE_TEMPLATE: &str = include_str!("../prompts/project_guide.md");
/// Hint template for when the project guide is missing.
const GUIDE_MISSING_TEMPLATE: &str = include_str!("../prompts/project_guide_missing.md");
/// Subagent guidance, injected **only for the top-level agent when the `subagent` tool is enabled**.
/// A child agent has no `subagent` tool (structural recursion guard), so telling it about one would
/// be a factual lie; hence this block is conditional.
const SUBAGENT_GUIDE_TEMPLATE: &str = include_str!("../prompts/subagent_guide.md");

/// Resolve the coding agent's workspace root.
///
/// Reads the `SHIRLEY_WORKSPACE` env var first (consistent with the `bash` tool, to avoid drift between the two),
/// otherwise falls back to the current directory. The path is canonicalized; on failure it is returned as is, for the prompt to describe truthfully.
pub fn workspace_root() -> PathBuf {
    let raw = std::env::var("SHIRLEY_WORKSPACE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// Read the project guide (`Agent.md`) in the workspace. Returns `None` if it does not exist or cannot be read.
pub fn guide_path(root: &Path) -> PathBuf {
    root.join(GUIDE_FILE_NAME)
}

fn read_guide(root: &Path) -> Option<String> {
    let path = root.join(GUIDE_FILE_NAME);
    let content = std::fs::read_to_string(path).ok()?;
    (!content.trim().is_empty()).then_some(content)
}

/// Construct the application-side system prompt.
///
/// Returns a [`SystemPrompt`] whose function reads the current working directory and `Agent.md` on each resolve.
/// This way the system prompt regenerated during compression rebuild always reflects the latest project state.
pub fn build(working_dir: PathBuf) -> SystemPrompt {
    SystemPrompt::from(move |context: &SystemPromptContext| {
        let root = context
            .working_dir
            .clone()
            .unwrap_or_else(|| working_dir.clone());
        render(&root)
    })
}

/// Like [`build`], but decides whether to inject the subagent guidance.
///
/// `subagents = true` is passed only for the **top-level** agent when the `subagent` tool is
/// registered; child agents (and a disabled config) get `false`, so the guidance never appears
/// where the tool does not exist.
pub fn build_with_subagents(working_dir: PathBuf, subagents: bool) -> SystemPrompt {
    SystemPrompt::from(move |context: &SystemPromptContext| {
        let root = context
            .working_dir
            .clone()
            .unwrap_or_else(|| working_dir.clone());
        render_with_subagents(&root, subagents)
    })
}

/// Assemble the role, working directory, and project guide into the final text.
///
/// Split out so it is independently testable: no dependency on the closure or runtime context.
fn render(root: &Path) -> String {
    // Default: inject the subagent guidance (the top-level agent). Tests that do not care about
    // subagents go through here and still get the full prompt.
    render_with_subagents(root, true)
}

/// Assemble the final prompt. `subagents` controls whether the subagent guidance block is injected.
fn render_with_subagents(root: &Path, subagents: bool) -> String {
    // Project guide block: inline the original text if present, otherwise give a "explore on your own first" hint.
    let guide_block = match read_guide(root) {
        Some(guide) => render_template(
            GUIDE_TEMPLATE,
            &[("guide_file", GUIDE_FILE_NAME), ("guide", &guide)],
        ),
        None => render_template(GUIDE_MISSING_TEMPLATE, &[("guide_file", GUIDE_FILE_NAME)]),
    };

    // Subagent guidance is only injected where the `subagent` tool exists; otherwise the placeholder
    // renders to the empty string (the renderer keeps unknown placeholders verbatim, so an empty
    // value is passed explicitly).
    let subagent_block = if subagents {
        SUBAGENT_GUIDE_TEMPLATE.to_owned()
    } else {
        String::new()
    };

    let working_dir = root.display().to_string();
    render_template(
        SYSTEM_TEMPLATE,
        &[
            ("working_dir", &working_dir),
            ("project_guide", &guide_block),
            ("subagent_guide", &subagent_block),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_role_and_working_dir() {
        let dir = tempfile_dir("render_role");
        let text = render(&dir);
        assert!(text.contains("You are Shirley"), "should contain the role setting: {text}");
        assert!(
            text.contains(&dir.display().to_string()),
            "should contain the working directory: {text}"
        );
    }

    #[test]
    fn render_instructs_todo_ledger_usage() {
        // A multi-step task should be broken down first, and each step's status and conclusion recorded with the todo ledger.
        // This assertion prevents this instruction from being lost in a future prompt edit.
        let dir = tempfile_dir("render_todo");
        let text = render(&dir);
        assert!(text.contains("`todo` tool"), "should instruct using the todo tool: {text}");
        assert!(text.contains("break it down"), "should break the task down first: {text}");
    }

    #[test]
    fn render_keeps_preamble_in_same_message_as_tool_calls() {
        // The preamble must be written in the **same** assistant message as the tool calls, not split across two turns:
        // a message without tool_calls ends the turn (the runtime's is_finished check),
        // so sending the preamble on its own would make the ReAct loop "stop for no reason" mid-exploration.
        // This assertion prevents this instruction from being lost in a future prompt edit.
        let dir = tempfile_dir("render_content_first");
        let text = render(&dir);
        assert!(
            text.contains("SAME assistant message"),
            "should require the preamble and tool calls in the same message: {text}"
        );
        assert!(
            text.contains("ends your turn"),
            "should explain that a message without tool_calls ends the turn: {text}"
        );
    }

    #[test]
    fn render_includes_guide_when_present() {
        let dir = tempfile_dir("render_guide");
        std::fs::write(dir.join(GUIDE_FILE_NAME), "# Project notes\nrun cargo test first").unwrap();
        let text = render(&dir);
        assert!(text.contains("run cargo test first"), "should inline Agent.md: {text}");
    }

    #[test]
    fn render_instructs_tool_environment_is_authoritative() {
        // Business-side correction (the figma special-case sentence in the original SDK prompt, migrated to the app layer in Step 7).
        // Prevents it from being lost in a future prompt edit.
        let dir = tempfile_dir("render_tool_env");
        let text = render(&dir);
        assert!(
            text.contains("do not mistake the model service"),
            "should state that the tool execution environment is authoritative: {text}"
        );
    }

    #[test]
    fn render_injects_subagent_guide_only_when_enabled() {
        // The guidance must appear exactly where the `subagent` tool exists (top level, enabled),
        // and be absent for child agents (which have no such tool).
        let dir = tempfile_dir("render_subagent");
        let with = render_with_subagents(&dir, true);
        assert!(
            with.contains("`subagent` tool"),
            "top level should get the subagent guidance: {with}"
        );
        let without = render_with_subagents(&dir, false);
        assert!(
            !without.contains("`subagent` tool"),
            "child agent must not get the subagent guidance: {without}"
        );
        // The placeholder must be fully substituted, never left verbatim.
        assert!(!with.contains("{{subagent_guide}}"), "placeholder must be substituted");
        assert!(!without.contains("{{subagent_guide}}"), "placeholder must be substituted");
    }

    #[test]
    fn render_notes_missing_guide() {
        let dir = tempfile_dir("render_missing");
        let text = render(&dir);
        assert!(text.contains("not found"), "should give a hint when the guide is missing: {text}");
    }

    #[test]
    fn build_resolves_context_dir_over_default() {
        // When the context explicitly gives a directory, it should win over the construction-time default.
        let default_dir = tempfile_dir("build_default");
        let ctx_dir = tempfile_dir("build_ctx");
        let prompt = build(default_dir);
        let resolved = prompt.resolve(&SystemPromptContext {
            working_dir: Some(ctx_dir.clone()),
        });
        assert!(
            resolved.contains(&ctx_dir.display().to_string()),
            "should use the context directory: {resolved}"
        );
    }

    #[test]
    fn build_falls_back_to_default_dir() {
        let default_dir = tempfile_dir("build_fallback");
        let prompt = build(default_dir.clone());
        let resolved = prompt.resolve(&SystemPromptContext { working_dir: None });
        assert!(
            resolved.contains(&default_dir.display().to_string()),
            "should fall back to the default directory without context: {resolved}"
        );
    }

    /// Create a unique temporary directory to avoid cross-test pollution.
    fn tempfile_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shirley_prompt_{tag}_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
}
