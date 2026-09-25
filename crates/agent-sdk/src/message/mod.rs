use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}
