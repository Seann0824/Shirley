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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        /// 思考块的完整性凭据（Anthropic `thinking` block 的 `signature`）。
        ///
        /// **协议中立**：它是"这段思考出自模型、未被篡改"的凭据，不是 Anthropic
        /// 私货。Anthropic 要求把 thinking 块原样回传时带上它，否则 400；
        /// 另两个协议不使用它（编码时忽略）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(default)]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: Option<String>,
    },
    // 不展示，转换的时候转换成 system
    ContextSummary {
        content: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

// 关注输入多少，输出多少，缓存命中多少，推理 token 花了多少。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    // None 表示供应商没上报，区别于上报了 0
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    // 累计命中率只统计上报了缓存数据的调用；旧数据缺此字段时按单次输入计算。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_reported_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

impl Usage {
    fn reported_input_tokens(&self) -> Option<u64> {
        self.cache_reported_input_tokens
            .or_else(|| self.cached_input_tokens.map(|_| self.input_tokens))
    }

    pub fn cached_tokens(&self) -> u64 {
        self.cached_input_tokens.unwrap_or(0)
    }

    pub fn missed_input_tokens(&self) -> u64 {
        self.input_tokens.saturating_sub(self.cached_tokens())
    }

    // 未上报时返回 None，不要把"测不到"当成 0% 命中
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let reported_input = self.reported_input_tokens()?;
        if reported_input == 0 {
            return None;
        }
        self.cached_input_tokens
            .map(|cached| cached as f64 / reported_input as f64)
    }
}

impl std::ops::Add for Usage {
    type Output = Usage;

    fn add(self, rhs: Usage) -> Usage {
        let reported_input = match (self.reported_input_tokens(), rhs.reported_input_tokens()) {
            (Some(a), Some(b)) => Some(a + b),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        Usage {
            input_tokens: self.input_tokens + rhs.input_tokens,
            output_tokens: self.output_tokens + rhs.output_tokens,
            cache_reported_input_tokens: reported_input,
            // 一侧未上报就保留已知的那侧，不能当 0 累加
            cached_input_tokens: match (self.cached_input_tokens, rhs.cached_input_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
            reasoning_tokens: match (self.reasoning_tokens, rhs.reasoning_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
        }
    }
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
                reasoning_content,
                thinking_signature: _,
                tool_calls,
            } => {
                if let Some(reasoning) = reasoning_content {
                    writeln!(f, "\u{1f9e0} Thinking: {}", reasoning)?;
                }
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
            Message::ContextSummary { content } => {
                let _ = write!(f, "🖥️ Context Summary: {}", content);
                Ok(())
            }
        }
    }
}
