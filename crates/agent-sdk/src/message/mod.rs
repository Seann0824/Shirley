use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "role", rename_all = "snake_case")]
enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: String,
    },
    Tool {
        tool_call_id: String,
        content: Option<String>,
    },
}
