// Anthropic Messages protocol DTO. Fields follow the `POST /v1/messages` contract.
//
// References (from real captured traffic, see `docs/anthropic-messages-api.md`):
// - endpoint `https://api.deepseek.com/anthropic/v1/messages`
// - headers `x-api-key` + `anthropic-version`
//
// Keeping unconsumed fields is part of protocol completeness, not dead code.
#![allow(dead_code)]

use serde::Deserialize;

/// Top-level message object (`type: "message"`).
#[derive(Debug, Clone, Deserialize)]
pub struct MessageResponse {
    #[serde(default)]
    pub id: String,
    /// `assistant`。
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// `end_turn` / `tool_use` / `max_tokens` / `stop_sequence` /
    /// `refusal` / `pause_turn`。
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// A content block in the response.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    /// Thinking block. `signature` is an integrity token and must be included when echoing back.
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    /// Tool call. `input` is a **JSON object** (not a string).
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    /// Other block types (`tool_result` appears on the request side, `redacted_thinking`, etc.) are ignored.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    /// **Does not include cache reads** (real captured traffic: `134 + 2301 = 2435`).
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

/// Streaming event. No `data: [DONE]`; ends with `message_stop`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Carries the initial usage (input side).
    MessageStart {
        message: MessageStartPayload,
    },
    /// Block start. `content_block` carries the initial content (a tool_use's id/name is here).
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    /// Block delta.
    ContentBlockDelta {
        index: usize,
        delta: Delta,
    },
    ContentBlockStop {
        index: usize,
    },
    /// **The terminal stop_reason and output usage are here.**
    MessageDelta {
        delta: MessageDeltaPayload,
        #[serde(default)]
        usage: Option<Usage>,
    },
    MessageStop,
    /// `ping` and other events are ignored.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageStartPayload {
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageDeltaPayload {
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    #[serde(rename = "text_delta")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "thinking_delta")]
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(rename = "signature_delta")]
    Signature {
        #[serde(default)]
        signature: String,
    },
    /// A JSON fragment of tool arguments; must be concatenated by index.
    #[serde(rename = "input_json_delta")]
    InputJson {
        #[serde(default)]
        partial_json: String,
    },
    #[serde(other)]
    Other,
}
