//! `dispatch_task` / `task_note`: how a bot hands long work to the background and adds to it.

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

use crate::pool::desk::TaskDesk;

pub struct DispatchTaskTool {
    desk: TaskDesk,
    bot: String,
    definition: ToolDefinition,
}

impl DispatchTaskTool {
    pub fn new(desk: TaskDesk, bot: impl Into<String>) -> Self {
        Self {
            desk,
            bot: bot.into(),
            definition: ToolDefinition {
                name: "dispatch_task".into(),
                description: "Hand a piece of work to a background worker and keep talking with \
                    the user. Use it for anything that takes more than a few steps: research, \
                    producing a file, a plan, code changes. The worker does NOT see this \
                    conversation, so the brief must stand on its own: the goal, every detail the \
                    user gave, constraints and what to deliver. The result appears to the user as \
                    a card in this conversation when it is done. After calling it, tell the user \
                    in one short sentence that it is under way."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "A short label for the card, in the user's language (e.g. \"云南自驾路线图\")."
                        },
                        "brief": {
                            "type": "string",
                            "description": "The complete, self-contained instruction for the worker."
                        }
                    },
                    "required": ["title", "brief"],
                    "additionalProperties": false
                }),
            },
        }
    }
}

impl Tool for DispatchTaskTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        Box::pin(async move {
            let args: DispatchArgs = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            let task = self
                .desk
                .dispatch(&self.bot, &args.title, &args.brief)
                .await
                .map_err(ToolError::ArgumentsError)?;
            Ok(serde_json::json!({
                "task_id": task.id,
                "title": task.title,
                "status": "queued",
            }))
        })
    }
}

#[derive(serde::Deserialize)]
struct DispatchArgs {
    title: String,
    brief: String,
}

pub struct TaskNoteTool {
    desk: TaskDesk,
    bot: String,
    definition: ToolDefinition,
}

impl TaskNoteTool {
    pub fn new(desk: TaskDesk, bot: impl Into<String>) -> Self {
        Self {
            desk,
            bot: bot.into(),
            definition: ToolDefinition {
                name: "task_note".into(),
                description: "Add to a task you dispatched earlier, when the user's message is \
                    about that work (\"also add a budget\", \"make it shorter\"). A running task \
                    picks the note up as soon as its current attempt ends; a delivered one is \
                    redone with it. Ids are listed in <tasks>. If it is unclear which task the \
                    user means, ask instead of guessing."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string", "description": "The task's id from <tasks>." },
                        "note": { "type": "string", "description": "What to add or change, self-contained." }
                    },
                    "required": ["task_id", "note"],
                    "additionalProperties": false
                }),
            },
        }
    }
}

impl Tool for TaskNoteTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        Box::pin(async move {
            let args: NoteArgs = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            let task = self
                .desk
                .note(&self.bot, &args.task_id, &args.note)
                .await
                .map_err(ToolError::ArgumentsError)?;
            Ok(serde_json::json!({
                "task_id": task.id,
                "title": task.title,
                "status": task.status,
            }))
        })
    }
}

#[derive(serde::Deserialize)]
struct NoteArgs {
    task_id: String,
    note: String,
}
