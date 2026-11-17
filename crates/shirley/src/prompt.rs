//! Application-side system prompt construction.
//!
//! The SDK only provides the mechanism ("the prompt can be a function of the runtime context",
//! `shirley_agent_sdk::SystemPrompt`); what the prompt says is a product decision and lives here.
//! The text is maintained in `prompts/*.md`; this file only picks templates and fills
//! placeholders.
//!
//! There are two prompts, because a bot does two different jobs:
//!
//! - **conversation** (`prompts/conversation.md`): the bot talking with the user. Short replies,
//!   hand longer work off with `dispatch_task`, never narrate tool use.
//! - **worker** (`prompts/worker.md`): an agent doing a dispatched task (or a subagent) in the
//!   background. Plans with `todo`, reads files economically, may use subagents and computer use.
//!
//! Both share the memory, file-showing and workspace blocks.

use std::path::{Path, PathBuf};

use shirley_agent_sdk::{SystemPrompt, SystemPromptContext};

use crate::prompt_template::render as render_template;

/// Guide file name. When a bot's workspace contains one, its text is inlined into worker prompts.
pub const GUIDE_FILE_NAME: &str = "Agent.md";

const CONVERSATION_TEMPLATE: &str = include_str!("../prompts/conversation.md");
const WORKER_TEMPLATE: &str = include_str!("../prompts/worker.md");
const MEMORY_BLOCK: &str = include_str!("../prompts/memory.md");
const FILES_BLOCK: &str = include_str!("../prompts/files.md");
const WORKSPACE_TEMPLATE: &str = include_str!("../prompts/workspace.md");
const GUIDE_TEMPLATE: &str = include_str!("../prompts/project_guide.md");
/// Subagent guidance: only where the `subagent` tool exists (a top-level worker).
const SUBAGENT_GUIDE_TEMPLATE: &str = include_str!("../prompts/subagent_guide.md");
/// Computer-use guidance: only for workers, and only where the `computer_*` tools work.
const COMPUTER_TEMPLATE: &str = include_str!("../prompts/computer.md");
/// Identity block used when a bot declares no `system_prompt_extra` of its own.
const DEFAULT_IDENTITY: &str = "You are Shirley, a personal agent running on the user's computer.";

/// Which job the prompt is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The bot talking with the user.
    Conversation,
    /// Background work; `subagents` when the `subagent` tool is registered.
    Worker { subagents: bool },
}

/// Build the system prompt for `role`. `identity` is the bot's persona
/// (`BotSpec::system_prompt_extra`); `None` uses the platform default.
pub fn build(working_dir: PathBuf, role: Role, identity: Option<String>) -> SystemPrompt {
    SystemPrompt::from(move |context: &SystemPromptContext| {
        let root = context
            .working_dir
            .clone()
            .unwrap_or_else(|| working_dir.clone());
        render(&root, role, identity.as_deref())
    })
}

fn read_guide(root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(root.join(GUIDE_FILE_NAME)).ok()?;
    (!content.trim().is_empty()).then_some(content)
}

/// Assemble the final text. Split out so it is testable without the closure.
fn render(root: &Path, role: Role, identity: Option<&str>) -> String {
    let identity = identity
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(DEFAULT_IDENTITY);
    let working_dir = root.display().to_string();
    let workspace = render_template(WORKSPACE_TEMPLATE, &[("working_dir", &working_dir)]);
    match role {
        Role::Conversation => render_template(
            CONVERSATION_TEMPLATE,
            &[
                ("identity", identity),
                ("memory", MEMORY_BLOCK),
                ("files", FILES_BLOCK),
                ("workspace", &workspace),
            ],
        ),
        Role::Worker { subagents } => {
            let guide = read_guide(root).map_or_else(String::new, |guide| {
                render_template(
                    GUIDE_TEMPLATE,
                    &[("guide_file", GUIDE_FILE_NAME), ("guide", &guide)],
                )
            });
            let subagent_guide = if subagents {
                format!("{SUBAGENT_GUIDE_TEMPLATE}\n")
            } else {
                String::new()
            };
            // Computer use works only on macOS; describing it elsewhere would send the model
            // chasing a capability it does not have.
            let computer = if cfg!(target_os = "macos") {
                format!("{COMPUTER_TEMPLATE}\n")
            } else {
                String::new()
            };
            render_template(
                WORKER_TEMPLATE,
                &[
                    ("identity", identity),
                    ("subagent_guide", &subagent_guide),
                    ("computer", &computer),
                    ("memory", MEMORY_BLOCK),
                    ("files", FILES_BLOCK),
                    ("workspace", &workspace),
                    ("project_guide", &guide),
                ],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKER: Role = Role::Worker { subagents: true };

    fn tempfile_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shirley_prompt_{tag}_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn no_placeholder_is_left_unfilled() {
        let dir = tempfile_dir("placeholders");
        for role in [
            Role::Conversation,
            WORKER,
            Role::Worker { subagents: false },
        ] {
            let text = render(&dir, role, None);
            assert!(!text.contains("{{"), "{role:?}: {text}");
            assert!(text.contains("You are Shirley"), "{role:?}");
            assert!(text.contains(&dir.display().to_string()), "{role:?}");
            assert!(
                text.contains("do not mistake the model service"),
                "{role:?}"
            );
            assert!(text.contains("`recall`"), "{role:?}");
        }
    }

    #[test]
    fn the_conversation_talks_and_hands_off_but_never_narrates() {
        let text = render(
            &tempfile_dir("conversation"),
            Role::Conversation,
            Some("You are NONO."),
        );
        assert!(text.starts_with("You are NONO."), "{text}");
        assert!(
            text.contains("dispatch_task") && text.contains("task_note"),
            "{text}"
        );
        assert!(text.contains("Do not narrate your process"), "{text}");
        // Worker machinery stays out of the chat.
        for absent in [
            "`todo` tool",
            "`subagent` tool",
            "computer_snapshot",
            "SAME assistant message",
        ] {
            assert!(
                !text.contains(absent),
                "conversation mentions {absent}: {text}"
            );
        }
    }

    #[test]
    fn the_worker_plans_reads_economically_and_finishes() {
        let text = render(&tempfile_dir("worker"), WORKER, None);
        assert!(text.contains("`todo` tool"), "{text}");
        assert!(text.contains("ends your turn"), "{text}");
        assert!(text.contains("`grep` tool"), "{text}");
        assert!(!text.contains("dispatch_task"), "{text}");
    }

    #[test]
    fn subagent_guidance_only_where_the_tool_exists() {
        let dir = tempfile_dir("subagent");
        assert!(render(&dir, WORKER, None).contains("`subagent` tool"));
        assert!(!render(&dir, Role::Worker { subagents: false }, None).contains("`subagent` tool"));
    }

    #[test]
    fn computer_use_is_taught_only_to_workers_on_macos() {
        let text = render(&tempfile_dir("computer"), WORKER, None);
        assert_eq!(
            text.contains("computer_snapshot"),
            cfg!(target_os = "macos")
        );
    }

    #[test]
    fn a_workspace_guide_is_inlined_for_workers_only() {
        let dir = tempfile_dir("guide");
        std::fs::write(dir.join(GUIDE_FILE_NAME), "# Notes\nrun cargo test first").unwrap();
        assert!(render(&dir, WORKER, None).contains("run cargo test first"));
        assert!(!render(&dir, Role::Conversation, None).contains("run cargo test first"));
        let empty = tempfile_dir("no_guide");
        assert!(!render(&empty, WORKER, None).contains(GUIDE_FILE_NAME));
    }

    #[test]
    fn the_context_directory_wins_over_the_default() {
        let default_dir = tempfile_dir("build_default");
        let ctx_dir = tempfile_dir("build_ctx");
        let prompt = build(default_dir.clone(), Role::Conversation, None);
        let resolved = prompt.resolve(&SystemPromptContext {
            working_dir: Some(ctx_dir.clone()),
        });
        assert!(resolved.contains(&ctx_dir.display().to_string()));
        let fallback = prompt.resolve(&SystemPromptContext { working_dir: None });
        assert!(fallback.contains(&default_dir.display().to_string()));
    }
}
