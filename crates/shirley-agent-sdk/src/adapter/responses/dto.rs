// Responses protocol DTO: fields defined per the `POST /responses` contract, deserialization only.
//
// References:
// - https://api-docs.deepseek.com/guides/responses_api/
// - https://api-docs.deepseek.com/zh-cn/api/create-response
//
// Unconsumed fields (`id` / `total_tokens` etc.) are kept as part of protocol completeness, not
// dead code.
#![allow(dead_code)]

use serde::Deserialize;

/// Top-level response object (`object: "response"`).
///
/// Non-streaming returns it directly; streaming carries the same object inside
/// `response.completed` / `incomplete` / `failed` events.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub id: String,
    /// `in_progress` / `completed` / `incomplete` / `failed`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub output: Vec<OutputItem>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub incomplete_details: Option<IncompleteDetails>,
    /// The error object carried when `failed`, kept as-is for upper-layer display.
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IncompleteDetails {
    #[serde(default)]
    pub reason: Option<String>,
}

/// An entry of `output[]`. Dispatched by `type`.
///
/// Note: `function_call` is a **sibling item parallel to** `message`, not nested inside an
/// assistant message — this is the most fundamental structural difference between Responses and
/// ChatCompletions.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputItem {
    Message(MessageItem),
    Reasoning(ReasoningItem),
    FunctionCall(FunctionCallItem),
    /// Other item types (`custom_tool_call` / `web_search_call` etc.) are ignored.
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
    /// Reasoning summary (the `summary_text` block). OpenAI-family reasoning models return only
    /// this by default; `content` (the raw CoT) is usually empty — see `ReasoningItem.summary`
    /// (a `required` field in the schema).
    #[serde(default)]
    pub summary: Option<Content>,
    /// Raw reasoning text (the `reasoning_text` block). Some implementations (e.g. DeepSeek) use this.
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

/// Content may be either a plain string or an array of content blocks (`input_text` /
/// `output_text` / `reasoning_text` / `input_image` …).
///
/// Decoding only cares about text, so only each block's `text` is extracted; non-text blocks
/// naturally yield an empty string.
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

/// Streaming events.
///
/// Unlike ChatCompletions: **there is no `data: [DONE]`**; the stream ends with
/// `response.completed` / `incomplete` / `failed`, so the terminal state is driven by event type.
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
    /// The **summary** delta of OpenAI-family reasoning models (`summary_text`), parallel to
    /// `reasoning_text.delta`; the field is `summary_index` rather than `content_index`, but we
    /// only care about `delta`.
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
    /// Other events (`created` / `in_progress` / `output_item.added` / `content_part.*` /
    /// `*.done` / `function_call_arguments.delta` …) are ignored: the terminal state is taken from
    /// the full object carried by `response.completed`, so there is no need to assemble from deltas.
    #[serde(other)]
    Other,
}
