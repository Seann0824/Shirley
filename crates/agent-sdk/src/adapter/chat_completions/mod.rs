mod dto;

use reqwest::header::HeaderValue;
use serde_json::{Value, map};

use crate::{
    ModelConfig,
    adapter::{ModelError, ModelRequest, ModelResponse, ModelfinishReaon, PreparedRequest},
    message, tool,
};

// 说实话我感觉这个应该是个 message 侧做的转换逻辑，而不是我们写在适配层对于这个消息处理。
fn encode_messages(messages: &[message::Message]) -> Vec<Value> {
    let encode_message = |msg: &message::Message| {
        let value = match msg {
            message::Message::User { content } => {
                serde_json::json!({
                    "role": "user",
                    "content": content,
                })
            }
            message::Message::Assistant {
                content,
                tool_calls,
            } => {
                let mut value = serde_json::json!({
                    "role": "assistant",
                    "content": content,
                });
                if !tool_calls.is_empty() {
                    value["tool_calls"] = serde_json::json!(
                        tool_calls
                            .iter()
                            .map(|call| {
                                serde_json::json!({
                                    "id": &call.id,
                                    "type": "function",
                                    "function": {
                                        "name": &call.name,
                                        "arguments": &call.arguments,
                                    },
                                })
                            })
                            .collect::<Vec<_>>()
                    );
                }

                value
            }
            message::Message::System { content } => {
                serde_json::json!({
                    "role": "system",
                    "content": content,
                })
            }

            message::Message::Tool {
                tool_call_id,
                content,
            } => {
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content,
                })
            }
        };
        value
    };
    messages.iter().map(encode_message).collect()
}

// 这里把工具转换成 API 所需要的格式
fn encode_tools(tools: &[&tool::ToolDefinition]) -> Vec<Value> {
    let encode_tool = |tool: &&tool::ToolDefinition| {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": &tool.name,
                "description": &tool.description,
                "parameters": &tool.parameters
            }
        })
    };

    tools.iter().map(encode_tool).collect()
}

pub fn encode_request(
    config: &ModelConfig,
    input: &ModelRequest<'_>,
) -> Result<PreparedRequest, ModelError> {
    // 处理消息列表，转换逻辑
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    if let Some(api_key) = &config.api_key {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| "API Key 无法构成有效请求头".to_owned())?;
        authorization.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
    }
    //  todo: 这里目前没有处理工具注入逻辑，以及思考逻辑。
    let body = serde_json::json!({
        "model": &config.model,
        // 这里应该转换对吧，但是看情况，我们现在主要以ChatCompletion 为唯一协议，其他协议都是根据这个协议适配过去的
        "messages": encode_messages(&input.messages),
        "tools": encode_tools(&input.tools),
        // 先不管思考模式了，后面补上，先跑通再说
        "thinking": {
            "type": "enabled",
        },
        "reasoning_effort": "low",
        "stream": false,
    });

    // 2. 处理工具
    Ok(PreparedRequest {
        url: config.base_url.clone(),
        headers,
        body,
    })
}

pub fn decode_response(body: serde_json::Value) -> Result<ModelResponse, ModelError> {
    let response = serde_json::from_value::<dto::ModelResponse>(body.clone())
        .map_err(|err| format!("响应解析失败: {err}"))?;

    // 开始转换 Message::Assistant and Message::Tool and finish_reason
    let choice = response
        .choices
        .get(0)
        .ok_or_else(|| "响应缺少 choices[0]".to_owned())?;

    let msg = &choice.message;
    let role = &msg.role;
    if role != "assistant" {
        return Err(format!("响应 role 不合法: {role}").into());
    }
    let content = &msg.content;
    // 我需要在这判断如果为空则返回 [], 否者就处理成 Message::Tool
    let tool_calls = msg
        .tool_calls
        .as_ref()
        .map(|calls| {
            calls
                .iter()
                .map(|call| message::ToolCall {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    arguments: call.function.arguments.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let finish_reason = choice
        .finish_reason
        .as_ref()
        .map(|reason| match reason.as_str() {
            "stop" => ModelfinishReaon::Stop,
            "tool_calls" => ModelfinishReaon::ToolCalls,
            "length" => ModelfinishReaon::Length,
            other => ModelfinishReaon::Other(other.to_owned()),
        })
        .unwrap_or(ModelfinishReaon::Other("unknown".to_owned()));

    // usage 需要转换成 message::Usage
    let usage = message::Usage {
        input_tokens: response.usage.prompt_tokens,
        output_tokens: response.usage.completion_tokens,
        cahced_input_tokens: response
            .usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens.map(|t| t)),
        reasoning_tokens: response
            .usage
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens.map(|t| t)),
    };

    Ok(ModelResponse {
        message: message::Message::Assistant {
            content: content.clone(),
            tool_calls: tool_calls.into(),
        },
        finish_reason,
        usage,
    })
}
