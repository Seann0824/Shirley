// 定义

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelResponse {
    pub id: String,
    pub choices: Vec<Choice>,
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
