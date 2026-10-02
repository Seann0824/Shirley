mod dto;

use std::{collections::BTreeMap, pin::Pin};

use futures::StreamExt;
use reqwest::header::HeaderValue;
use serde_json::Value;

use crate::{
    ModelConfig,
    adapter::{
        AdapterError, AdapterEvent, ModelRequest, ModelResponse, ModelFinishReason,
        PreparedRequest,
    },
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
                reasoning_content,
                tool_calls,
            } => {
                let mut value = serde_json::json!({
                    "role": "assistant",
                    "content": content,
                });
                if let Some(reasoning) = reasoning_content {
                    value["reasoning_content"] = serde_json::json!(reasoning);
                }
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
            message::Message::ContextSummary { content } => {
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
) -> Result<PreparedRequest, AdapterError> {
    // 处理消息列表，转换逻辑
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    if let Some(api_key) = &config.api_key {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| AdapterError::Encode("API Key 无法构成有效请求头".into()))?;
        authorization.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
    }
    //  todo: 这里目前没有处理工具注入逻辑，以及思考逻辑。
    let thinking = match config.thinking {
        true => "enabled",
        false => "disabled",
    };

    let body = serde_json::json!({
        "model": &config.model,
        // 这里应该转换对吧，但是看情况，我们现在主要以ChatCompletion 为唯一协议，其他协议都是根据这个协议适配过去的
        "messages": encode_messages(&input.messages),
        "tools": encode_tools(&input.tools),
        "thinking": {
            "type": thinking,
        },
        "reasoning_effort": &config.reasoning_effort,
        "stream": &config.stream,
    });

    // 2. 处理工具
    Ok(PreparedRequest {
        url: config.base_url.clone(),
        headers,
        body,
    })
}

pub fn decode_response(body: serde_json::Value) -> Result<ModelResponse, AdapterError> {
    let response = serde_json::from_value::<dto::ModelResponse>(body.clone())
        .map_err(|err| AdapterError::Decode(format!("响应解析失败: {err}")))?;

    // 开始转换 Message::Assistant and Message::Tool and finish_reason
    let choice = response
        .choices
        .get(0)
        .ok_or_else(|| AdapterError::Decode("响应缺少 choices[0]".to_owned()))?;

    let msg = &choice.message;
    let role = &msg.role;
    if role != "assistant" {
        return Err(AdapterError::Decode(format!(
            "响应 role 不合法: {role}"
        )));
    }
    let content = &msg.content;
    let reasoning_content = msg.reasoning_content.clone();
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
            "stop" => ModelFinishReason::Stop,
            "tool_calls" => ModelFinishReason::ToolCalls,
            "length" => ModelFinishReason::Length,
            other => ModelFinishReason::Other(other.to_owned()),
        })
        .unwrap_or(ModelFinishReason::Other("unknown".to_owned()));

    // usage 需要转换成 message::Usage
    let cached_input_tokens = response
        .usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|details| details.cached_tokens);
    let usage = message::Usage {
        input_tokens: response.usage.prompt_tokens,
        output_tokens: response.usage.completion_tokens,
        cached_input_tokens,
        cache_reported_input_tokens: cached_input_tokens.map(|_| response.usage.prompt_tokens),
        reasoning_tokens: response
            .usage
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens),
    };

    Ok(ModelResponse {
        message: message::Message::Assistant {
            content: content.clone(),
            reasoning_content,
            tool_calls: tool_calls.into(),
        },
        finish_reason,
        usage,
    })
}

fn merge_tool_call_deltas(
    tool_calls: &mut BTreeMap<usize, message::ToolCall>,
    deltas: impl IntoIterator<Item = dto::ToolCallDelta>,
) {
    for delta in deltas {
        let tool_call = tool_calls
            .entry(delta.index)
            .or_insert_with(|| message::ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
        if let Some(id) = delta.id {
            tool_call.id.push_str(&id);
        }
        if let Some(function) = delta.function {
            if let Some(name) = function.name {
                tool_call.name.push_str(&name);
            }
            if let Some(arguments) = function.arguments {
                tool_call.arguments.push_str(&arguments);
            }
        }
    }
}

fn finish_tool_calls(
    tool_calls: BTreeMap<usize, message::ToolCall>,
) -> Result<Vec<message::ToolCall>, AdapterError> {
    for (index, tool_call) in &tool_calls {
        if tool_call.id.is_empty() {
            return Err(AdapterError::Decode(format!(
                "流式工具调用 index={index} 缺少 id"
            )));
        }
        if tool_call.name.is_empty() {
            return Err(AdapterError::Decode(format!(
                "流式工具调用 index={index} 缺少函数名"
            )));
        }
    }
    Ok(tool_calls.into_values().collect())
}

//
pub async fn decode_stream_response(
    response: reqwest::Response,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>> {
    Box::pin(async_stream::try_stream! {
        let mut byte_stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut content = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls = BTreeMap::new();
        let mut usage = message::Usage::default();
        let mut finish_reason = None;
        while let Some(chunk) = byte_stream.next().await {
            // 读取中断不能 unwrap：它是网络错误，应该走 Transport 分类让上层决定重试。
            let chunk = chunk.map_err(AdapterError::Transport)?;
            let text = String::from_utf8_lossy(&chunk);
            buffer.push_str(&text);

            while let Some(pos) = buffer.find("\n\n") {
                // 我觉得这个json结构应该一样的吧？
                let section = buffer[..pos].to_string();
                // 去掉当前读取后的事件 和 两个换行符号
                buffer.drain(..pos + 2);

                let mut event = String::new();
                let mut data = String::new();
                // 解析每行的数据
                for line in section.lines() {
                    if let Some(rest) = line.strip_prefix("event:") {
                        // 一个个sse应该有对应event，但是看起来model好像没有遵循这个规范。
                        event = rest.trim().into();
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        data.push_str(rest.trim());
                    } else if line.strip_prefix("id:").is_some() {
                        // 事件 id 目前没有消费方（多用于断线续传），先忽略而不是 panic。
                        // 等真正实现断线重连时再在这里保留 last_event_id。
                    }
                }
                // 为啥会出现
                let payload = data.trim();
                if payload.is_empty() || payload == "[DONE]" {
                    continue;
                }
                let value = serde_json::from_str::<dto::ModelStreamResponse>(&data.trim()).map_err(|e| AdapterError::Decode(format!("SSE 反序列化失败: {e}")))?;
                if let Some(stream_usage) = value.usage {
                    let cached_input_tokens = stream_usage
                        .prompt_tokens_details
                        .as_ref()
                        .and_then(|details| details.cached_tokens);
                    usage = message::Usage {
                        input_tokens: stream_usage.prompt_tokens,
                        output_tokens: stream_usage.completion_tokens,
                        cached_input_tokens,
                        cache_reported_input_tokens: cached_input_tokens
                            .map(|_| stream_usage.prompt_tokens),
                        reasoning_tokens: stream_usage
                            .completion_tokens_details
                            .as_ref()
                            .and_then(|details| details.reasoning_tokens),
                    };
                }
                let Some(choice) = value.choices.first() else {
                    continue;
                };
                let delta = &choice.delta;
                let reasoning_delta = delta.reasoning_content.clone().unwrap_or(String::new());
                let content_delta = delta.content.clone().unwrap_or(String::new());

                if reasoning_delta.len() > 0 {
                    reasoning_content.push_str(&reasoning_delta);
                    yield AdapterEvent::ReasoningDelta(reasoning_delta);
                }
                if content_delta.len() > 0 {
                    content.push_str(&content_delta);
                    yield AdapterEvent::ContentDelta(content_delta);
                }
                // tool_calls 保存
                merge_tool_call_deltas(
                    &mut tool_calls,
                    delta.tool_calls.clone().unwrap_or_default(),
                );

                if let Some(reason) = &choice.finish_reason {
                    finish_reason = Some(match reason.as_str() {
                        "stop" => ModelFinishReason::Stop,
                        "tool_calls" => ModelFinishReason::ToolCalls,
                        "length" => ModelFinishReason::Length,
                        other => ModelFinishReason::Other(other.to_owned()),
                    });
                }
                // 这里必然返回 assistant， 所以接下来我们就是要把数据向外yield
            }
        }
        if let Some(finish_reason) = finish_reason {
            let tool_calls = finish_tool_calls(tool_calls)?;
            yield AdapterEvent::Finished(ModelResponse {
                message: message::Message::Assistant {
                    content: (!content.is_empty()).then_some(content),
                    reasoning_content: (!reasoning_content.is_empty()).then_some(reasoning_content),
                    tool_calls,
                },
                finish_reason,
                usage,
            });
        }
    })
}
