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

// 关注输入多少，输出多少，缓存命中多少，推理 token 花了多少。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cahced_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

use std::fmt;

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::User { content } => {
                let _ = write!(f, "👤 User: {}", content);
                Ok(())
            }
            Message::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(content) = content {
                    writeln!(f, "🤖 Assistant: {}", content)?;
                }

                for call in tool_calls {
                    writeln!(f, "🔧 Tool Call: {}({})", call.name, call.arguments)?;
                }

                Ok(())
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                let _ = write!(
                    f,
                    "⚙️ Tool Result [{}]: {}",
                    tool_call_id,
                    content.as_deref().unwrap_or("")
                );
                Ok(())
            }
            Message::System { content } => {
                let _ = write!(f, "🖥️ System: {}", content);
                Ok(())
            }
        }
    }
}
