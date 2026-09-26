// 定义

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelResponse {
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
    // 通过Value 反序列化，需要重名ming，避免与函数名冲突。
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
pub struct CompletionTokensDetails {
    pub reasoning_tokens: Option<u64>,
    pub accepted_prediction_tokens: Option<u64>,
    pub rejected_prediction_tokens: Option<u64>,
    pub audio_tokens: Option<u64>,
    pub text_tokens: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct PromptTokensDetails {
    pub audio_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub text_tokens: Option<u64>,
    pub image_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}
