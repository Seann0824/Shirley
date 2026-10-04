// Responses 协议 DTO：字段按 `POST /responses` 契约定义，只做反序列化。
//
// 依据：
// - https://api-docs.deepseek.com/guides/responses_api/
// - https://api-docs.deepseek.com/zh-cn/api/create-response
//
// 未消费的字段（`id` / `total_tokens` 等）保留是协议完整性的一部分，不是死代码。
#![allow(dead_code)]

use serde::Deserialize;

/// 顶层响应对象（`object: "response"`）。
///
/// 非流式直接是它；流式则由 `response.completed` / `incomplete` / `failed`
/// 事件携带同一个对象。
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub id: String,
    /// `in_progress` / `completed` / `incomplete` / `failed`。
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub output: Vec<OutputItem>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub incomplete_details: Option<IncompleteDetails>,
    /// `failed` 时携带的错误对象，原样保留供上层展示。
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IncompleteDetails {
    #[serde(default)]
    pub reason: Option<String>,
}

/// `output[]` 的条目。按 `type` 分派。
///
/// 注意：`function_call` 是与 `message` **并列的兄弟 item**，而不是嵌在
/// assistant 消息里——这是 Responses 与 ChatCompletions 最本质的结构差异。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputItem {
    Message(MessageItem),
    Reasoning(ReasoningItem),
    FunctionCall(FunctionCallItem),
    /// 其余 item 类型（`custom_tool_call` / `web_search_call` 等）忽略。
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageItem {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<Content>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReasoningItem {
    /// 推理摘要（`summary_text` 块）。OpenAI 系推理模型默认只回这个，
    /// `content`（原始 CoT）通常为空——见 `ReasoningItem.summary`（schema 里是
    /// `required` 字段）。
    #[serde(default)]
    pub summary: Option<Content>,
    /// 原始推理文本（`reasoning_text` 块）。部分实现（如 DeepSeek）走这里。
    #[serde(default)]
    pub content: Option<Content>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCallItem {
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: String,
}

/// 内容既可能是纯字符串，也可能是内容块数组（`input_text` / `output_text` /
/// `reasoning_text` / `input_image` …）。
///
/// 解码时只关心文本，所以只提取每个块的 `text`，非文本块自然得到空串。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl Content {
    pub fn text(&self) -> String {
        match self {
            Content::Text(text) => text.clone(),
            Content::Parts(parts) => parts.iter().map(|part| part.text.as_str()).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentPart {
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub input_tokens_details: Option<InputTokensDetails>,
    #[serde(default)]
    pub output_tokens_details: Option<OutputTokensDetails>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InputTokensDetails {
    #[serde(default)]
    pub cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OutputTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
}

/// 流式事件。
///
/// 与 ChatCompletions 不同：**没有 `data: [DONE]`**，流以
/// `response.completed` / `incomplete` / `failed` 收尾，所以终态由事件类型驱动。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta {
        #[serde(default)]
        delta: String,
    },
    #[serde(rename = "response.reasoning_text.delta")]
    ReasoningTextDelta {
        #[serde(default)]
        delta: String,
    },
    /// OpenAI 系推理模型的**摘要**增量（`summary_text`），与 `reasoning_text.delta`
    /// 并列；字段是 `summary_index` 而非 `content_index`，但我们只关心 `delta`。
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta {
        #[serde(default)]
        delta: String,
    },
    #[serde(rename = "response.completed")]
    Completed { response: Response },
    #[serde(rename = "response.incomplete")]
    Incomplete { response: Response },
    #[serde(rename = "response.failed")]
    Failed { response: Response },
    /// 其余事件（`created` / `in_progress` / `output_item.added` /
    /// `content_part.*` / `*.done` / `function_call_arguments.delta` …）忽略：
    /// 终态以 `response.completed` 携带的完整对象为准，无需靠 delta 拼装。
    #[serde(other)]
    Other,
}
