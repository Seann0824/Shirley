//! The `todo` tool: the model's only entry point to maintain its own task ledger.
//!
//! Hand-written `Tool` impl (not the `#[tool]` macro): the state (`Arc<TodoStore>`) is held by
//! the tool itself and shared with [`TodoContextProvider`](crate::todo::TodoContextProvider),
//! which injects the ledger at the end of every request.
//!
//! The ledger model itself (rendering / patching / validation) lives in [`crate::todo`]; this
//! module only exposes it to the model.

use std::sync::Arc;

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

use crate::todo::{TodoStore, TodoUpdate};

/// The `todo` tool: the model's only entry point to maintain its own task ledger.
///
/// Hand-written `Tool` impl: the state (`Arc<TodoStore>`) is held by the tool itself, sharing the same `Arc`
/// with [`TodoContextProvider`].
pub struct TodoTool {
    store: Arc<TodoStore>,
    definition: ToolDefinition,
}

impl TodoTool {
    pub fn new(store: Arc<TodoStore>) -> Self {
        Self {
            store,
            definition: ToolDefinition {
                name: "todo".into(),
                description: "Maintain your own task ledger. For any multi-step task, break the \
                    work down and record it here BEFORE acting: set the goal and the step checklist. \
                    Mark exactly one step in_progress at a time as you work, mark it completed when \
                    done, and append findings (facts you have learned: file contents, command results, \
                    decisions) and open questions. The ledger is injected into every request and \
                    survives context compaction, so keeping it current prevents you from repeating \
                    work that was already done. Update it whenever you finish a step or learn \
                    something worth keeping."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "goal": {
                            "type": "string",
                            "description": "Set or replace the current task goal."
                        },
                        "steps": {
                            "type": "array",
                            "description": "Replace the full step checklist. Keep at most one step in_progress.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string", "description": "What this step does." },
                                    "status": {
                                        "type": "string",
                                        "enum": ["pending", "in_progress", "completed"],
                                        "default": "pending",
                                        "description": "Step state. At most one step may be in_progress at a time."
                                    }
                                },
                                "required": ["text"],
                                "additionalProperties": false
                            }
                        },
                        "add_findings": {
                            "type": "array",
                            "description": "Append facts you have learned (file contents, command results, decisions).",
                            "items": { "type": "string" }
                        },
                        "add_open_questions": {
                            "type": "array",
                            "description": "Append unresolved questions.",
                            "items": { "type": "string" }
                        },
                        "clear": {
                            "type": "boolean",
                            "description": "Reset the entire ledger."
                        }
                    },
                    "additionalProperties": false
                }),
            },
        }
    }
}

impl Tool for TodoTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        let store = self.store.clone();
        Box::pin(async move {
            let update: TodoUpdate = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            let rendered = store
                .apply(update)?
                .unwrap_or_else(|| "(task ledger is empty)".to_string());
            serde_json::to_value(format!("task ledger updated:\n{rendered}")).map_err(|error| {
                ToolError::ExecutionError(format!("failed to serialize tool result: {error}"))
            })
        })
    }
}
