// Protocol DTO: fields fully defined per the ChatCompletions contract, deserialization only.
// Some fields are currently unconsumed (e.g. `id` / `total_tokens` / the various token details);
// keeping them is part of protocol completeness, not dead code.
#![allow(
    dead_code,
    reason = "protocol DTO keeps unconsumed fields for wire completeness"
)]

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelResponse {
    #[serde(default)]
    pub id: String,
    pub choices: Vec<Choice>,
    pub usage: Usage,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Choice {
    pub finish_reason: Option<String>,
    pub message: ChoiceMessage,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChoiceMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    // Deserialized from Value, needs a rename to avoid clashing with the function name.
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub completion_tokens_details: Option<CompletionTokensDetails>,
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "field names are the wire contract; the `_tokens` postfix matches the OpenAI API"
)]
pub struct CompletionTokensDetails {
    pub reasoning_tokens: Option<u64>,
    pub accepted_prediction_tokens: Option<u64>,
    pub rejected_prediction_tokens: Option<u64>,
    pub audio_tokens: Option<u64>,
    pub text_tokens: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "field names are the wire contract; the `_tokens` postfix matches the OpenAI API"
)]
pub struct PromptTokensDetails {
    pub audio_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub text_tokens: Option<u64>,
    pub image_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelStreamResponse {
    #[serde(default)]
    pub id: String,
    pub choices: Vec<ChoiceStream>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChoiceStream {
    pub index: usize,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Delta {
    pub role: Option<String>,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub function: Option<FunctionCallDelta>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FunctionCallDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}
