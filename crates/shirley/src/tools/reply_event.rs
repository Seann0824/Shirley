//! `reply_to_event`: answer where a connector event came from (a Slack thread, a Feishu chat, a
//! GitHub issue). The event's brief carries the `reply_to` handle; the credentials stay with the
//! connection, read when the reply is sent.

use std::path::PathBuf;

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

pub struct ReplyToEventTool {
    global: PathBuf,
    definition: ToolDefinition,
}

impl ReplyToEventTool {
    pub fn new(global: PathBuf) -> Self {
        Self {
            global,
            definition: ToolDefinition {
                name: "reply_to_event".into(),
                description: "Answer an event from a connected service where it came from: a \
                    Slack thread, a Feishu chat, a GitHub issue or pull request. Use the reply_to \
                    handle the event gave you. Write as you would in that place: short, in the \
                    sender's language."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "reply_to": { "type": "string", "description": "The event's reply_to handle, exactly as given." },
                        "text": { "type": "string", "description": "The answer (Markdown where the service renders it)." }
                    },
                    "required": ["reply_to", "text"],
                    "additionalProperties": false
                }),
            },
        }
    }
}

impl Tool for ReplyToEventTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        let global = self.global.clone();
        Box::pin(async move {
            let field = |key: &str| {
                input
                    .get(key)
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| ToolError::ArgumentsError(format!("missing `{key}`")))
            };
            let (handle, text) = (field("reply_to")?, field("text")?);
            let sent = crate::connectors::reply(&global, &handle, &text)
                .await
                .map_err(ToolError::ExecutionError)?;
            Ok(serde_json::json!({ "sent": sent }))
        })
    }
}
