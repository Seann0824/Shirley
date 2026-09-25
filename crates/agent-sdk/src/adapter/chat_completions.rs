use std::{fmt::format, str::FromStr, todo};

use reqwest::header::{self, HeaderValue};
use serde_json::Value;

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
    let _id = body.get("id").and_then(|value| value.as_str());

    let choice = body
        .get("choices")
        .and_then(|value| value.as_array())
        .and_then(|choices| choices.first())
        .ok_or_else(|| "响应缺少 choices[0]".to_owned())?;

    // stream 模式下会出现没有 finish_reason
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "响应缺少 finish_reason".to_owned())?;

    // stop, length, content_filter, tool_calls, insufficient_system_resource, aborted
    let finish_reason = match finish_reason {
        "stop" => ModelfinishReaon::Stop,
        "tool_calls" => ModelfinishReaon::ToolCalls,
        "length" => ModelfinishReaon::Length,
        // 去
        other => ModelfinishReaon::Other(other.to_owned()),
    };

    let message = choice
        .get("message")
        .ok_or_else(|| "响应缺少 message".to_owned())?;

    let role = message
        .get("role")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "响应缺少 message.role".to_owned())?;

    if role != "assistant" {
        return Err(format!("模型响应角色不符合预期：{role}"));
    }

    // content 可以缺失或为 null，但其他类型明确报错。
    let content = match message.get("content") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(_) => {
            return Err("message.content 必须是字符串或 null".into());
        }
    };

    let tool_calls = match message.get("tool_calls") {
        // 没有工具调用时，使用空列表。
        None | Some(serde_json::Value::Null) => Vec::new(),

        Some(serde_json::Value::Array(calls)) => {
            let mut tool_calls = Vec::with_capacity(calls.len());

            for call in calls {
                let id = required_string(call, "id")?;
                let call_type = required_string(call, "type")?;

                if call_type != "function" {
                    return Err(format!("不支持的工具调用类型：{call_type}"));
                }

                let function = call
                    .get("function")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| "工具调用缺少 function 对象".to_owned())?;

                let name = required_string(function, "name")?;
                let arguments = required_string(function, "arguments")?;

                tool_calls.push(message::ToolCall {
                    id: id.to_owned(),
                    name: name.to_owned(),

                    // 保留原始参数文本，不重新序列化。
                    arguments: arguments.to_owned(),
                });
            }

            tool_calls
        }

        Some(_) => {
            return Err("message.tool_calls 必须是数组或 null".into());
        }
    };

    let message = message::Message::Assistant {
        content,
        tool_calls: tool_calls.into(),
    };

    Ok(ModelResponse {
        message,
        finish_reason,
    })
}

fn required_string<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, ModelError> {
    value
        .get(field)
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("字段 {field} 缺失或不是字符串"))
}
