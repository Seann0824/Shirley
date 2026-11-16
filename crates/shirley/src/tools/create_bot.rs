//! The `create_bot` tool: lets a bot define a new bot (identity + tool set + skills) by writing
//! its `spec.toml`. The new bot appears in the bot list immediately; its runtime is created on
//! first use.

use std::path::PathBuf;

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

use crate::bot::{BotSpec, platform_tools};

pub struct CreateBotTool {
    bots_root: PathBuf,
    definition: ToolDefinition,
}

impl CreateBotTool {
    pub fn new(bots_root: PathBuf) -> Self {
        Self {
            bots_root,
            definition: ToolDefinition {
                name: "create_bot".into(),
                description: "Create a new bot on this platform. A bot is a persona (system \
                    prompt) plus the capability tools and skills it may use. Use it when the user \
                    asks for a new assistant for a recurring kind of work. The bot is available \
                    right after this call; tell the user its name."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "URL-safe id: 1-32 lowercase letters, digits or '-', starting with a letter (e.g. \"clerk\")."
                        },
                        "display_name": {
                            "type": "string",
                            "description": "Human-readable name shown in the UI (e.g. \"Clerk\")."
                        },
                        "persona": {
                            "type": "string",
                            "description": "The bot's identity block: who it is, what it does, how it works. Written in second person (\"You are ...\")."
                        },
                        "tools": {
                            "type": "array",
                            "items": { "type": "string", "enum": platform_tools::DEFAULT_SET },
                            "description": "Capability tools the bot may use. Omit for the full default set."
                        },
                        "skills": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Names of installed skills the bot may activate. Omit for all."
                        }
                    },
                    "required": ["name", "display_name", "persona"],
                    "additionalProperties": false
                }),
            },
        }
    }
}

impl Tool for CreateBotTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        let bots_root = self.bots_root.clone();
        Box::pin(async move {
            let args: CreateBotArgs = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            let spec = BotSpec {
                name: args.name.trim().to_owned(),
                display_name: args.display_name.trim().to_owned(),
                tools: args.tools.unwrap_or_default(),
                skills: args.skills.unwrap_or_default(),
                system_prompt_extra: Some(args.persona.trim().to_owned()),
            };
            if spec.display_name.is_empty() || spec.system_prompt_extra.as_deref() == Some("") {
                return Err(ToolError::ArgumentsError(
                    "`display_name` and `persona` must not be empty".into(),
                ));
            }
            if bots_root.join(&spec.name).join("spec.toml").exists() {
                return Err(ToolError::ArgumentsError(format!(
                    "a bot named `{}` already exists",
                    spec.name
                )));
            }
            spec.save(&bots_root).map_err(ToolError::ExecutionError)?;
            Ok(serde_json::json!({
                "created": spec.name,
                "display_name": spec.display_name,
                "tools": spec.effective_tools(),
                "skills": spec.skills,
            }))
        })
    }
}

#[derive(serde::Deserialize)]
struct CreateBotArgs {
    name: String,
    display_name: String,
    persona: String,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    skills: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_create_bot_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn creates_a_spec_the_catalog_can_load() {
        let root = tmp("ok");
        let tool = CreateBotTool::new(root.clone());
        let out = tool
            .invoke(
                serde_json::json!({
                    "name": "clerk",
                    "display_name": "Clerk",
                    "persona": "You are Clerk, you tidy documents.",
                    "tools": ["read"]
                }),
                ToolContext::new(),
            )
            .await
            .unwrap();
        assert_eq!(out["created"], "clerk");
        let spec = BotSpec::load_or_default("clerk", &root);
        assert_eq!(spec.display_name, "Clerk");
        assert_eq!(spec.tools, vec!["read"]);
        assert!(spec.system_prompt_extra.unwrap().contains("Clerk"));
        assert!(BotSpec::list_all(&root).iter().any(|s| s.name == "clerk"));
    }

    #[tokio::test]
    async fn rejects_bad_names_and_duplicates() {
        let root = tmp("bad");
        let tool = CreateBotTool::new(root.clone());
        let bad = serde_json::json!({ "name": "Bad Name", "display_name": "x", "persona": "y" });
        assert!(matches!(
            tool.invoke(bad, ToolContext::new()).await.unwrap_err(),
            ToolError::ExecutionError(_)
        ));
        let ok = serde_json::json!({ "name": "dup", "display_name": "x", "persona": "y" });
        tool.invoke(ok.clone(), ToolContext::new()).await.unwrap();
        assert!(matches!(
            tool.invoke(ok, ToolContext::new()).await.unwrap_err(),
            ToolError::ArgumentsError(_)
        ));
    }
}
