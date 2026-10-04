// Anthropic Messages 协议 DTO。字段按 `POST /v1/messages` 契约定义。
//
// 依据（真实抓包，见 `docs/anthropic-messages-api.md`）：
// - 端点 `https://api.deepseek.com/anthropic/v1/messages`
// - 请求头 `x-api-key` + `anthropic-version`
//
// 未消费的字段保留是协议完整性的一部分，不是死代码。
#![allow(dead_code)]

use serde::Deserialize;

/// 顶层消息对象（`type: "message"`）。
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

/// 响应里的 content block。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    /// 思考块。`signature` 是完整性凭据，回传时必须带上。
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    /// 工具调用。`input` 是 **JSON 对象**（不是字符串）。
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    /// 其余块类型（`tool_result` 出现在请求侧、`redacted_thinking` 等）忽略。
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    /// **不含缓存读取**（真实抓包：`134 + 2301 = 2435`）。
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

/// 流式事件。无 `data: [DONE]`，以 `message_stop` 结束。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// 携带初始 usage（input 侧）。
    MessageStart { message: MessageStartPayload },
    /// 块开始。`content_block` 携带初始内容（tool_use 的 id/name 在这里）。
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    /// 块增量。
    ContentBlockDelta { index: usize, delta: Delta },
    ContentBlockStop { index: usize },
    /// **收尾的 stop_reason 与 output usage 在这里。**
    MessageDelta {
        delta: MessageDeltaPayload,
        #[serde(default)]
        usage: Option<Usage>,
    },
    MessageStop,
    /// `ping` 及其余事件忽略。
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
    /// 工具参数的 JSON 分片，需按 index 拼接。
    #[serde(rename = "input_json_delta")]
    InputJson {
        #[serde(default)]
        partial_json: String,
    },
    #[serde(other)]
    Other,
}
