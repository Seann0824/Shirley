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

// Honestly this conversion logic feels like it belongs on the message side, rather than being
// written here in the adapter layer for message handling.
fn encode_messages(messages: &[message::Message]) -> Vec<Value> {
    let encode_message = |msg: &message::Message| {
        match msg {
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
        }
    };
    messages.iter().map(encode_message).collect()
}

// Convert the tools into the format the API requires
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
    // Handle the message list; conversion logic
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    if let Some(api_key) = &config.api_key {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|error| AdapterError::Encode(format!("API key is not a valid header value: {error}")))?;
        authorization.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
    }
    super::attach_local_session_header(&mut headers, &config.base_url, input.session_id)?;
    let mut body = serde_json::json!({
        "model": &config.model,
        // This should convert, right? But it depends: we currently treat ChatCompletions as the
        // sole protocol, and the others are adapted from it.
        "messages": encode_messages(input.messages),
        "tools": encode_tools(input.tools),
        "stream": &config.stream,
    });

    // Optional fields: only written when Some, avoiding sending null to strictly validating endpoints.
    if let Some(temperature) = config.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    // Note the wire key is ChatCompletions' `max_tokens`, not Responses'
    // `max_output_tokens`. For variants like `max_completion_tokens`, use extra_body.
    if let Some(max_output_tokens) = config.max_output_tokens {
        body["max_tokens"] = serde_json::json!(max_output_tokens);
    }
    if let Some(reasoning_effort) = &config.reasoning_effort {
        body["reasoning_effort"] = serde_json::json!(reasoning_effort);
    }
    if let Some(tool_choice) = &config.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }
    // thinking is written only when enabled. `{"type":"enabled"}` is a private convention of some
    // vendors; sending `disabled` when off is rejected by most endpoints, so off omits it entirely.
    if config.thinking {
        body["thinking"] = serde_json::json!({ "type": "enabled" });
    }

    // The escape hatch is applied last: the app can override standard fields, add vendor-private
    // fields, or delete fields.
    if let Some(extra) = &config.extra_body {
        apply_extra_body(&mut body, extra);
    }

    Ok(PreparedRequest {
        url: config.base_url.clone(),
        headers,
        body,
    })
}

/// Shallow-merge `extra` into the request body.
///
/// - App keys override standard keys;
/// - A `null` value **deletes** that key (so the app can remove a field the SDK writes by default);
/// - A non-object `extra` is ignored (the request body stays unchanged).
fn apply_extra_body(body: &mut Value, extra: &Value) {
    let Value::Object(extra) = extra else {
        return;
    };
    let Value::Object(body) = body else {
        return;
    };
    for (key, value) in extra {
        if value.is_null() {
            body.remove(key);
        } else {
            body.insert(key.clone(), value.clone());
        }
    }
}

pub fn decode_response(body: serde_json::Value) -> Result<ModelResponse, AdapterError> {
    let response = serde_json::from_value::<dto::ModelResponse>(body.clone())
        .map_err(|err| AdapterError::Decode(format!("failed to parse response: {err}")))?;

    // Start converting Message::Assistant and Message::Tool and finish_reason
    let choice = response
        .choices
        .first()
        .ok_or_else(|| AdapterError::Decode("response is missing choices[0]".to_owned()))?;

    let msg = &choice.message;
    let role = &msg.role;
    if role != "assistant" {
        return Err(AdapterError::Decode(format!(
            "unexpected response role: {role}"
        )));
    }
    let content = &msg.content;
    let reasoning_content = msg.reasoning_content.clone();
    // Here I need to check: if empty, return [], otherwise process into Message::Tool
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

    // usage needs converting into message::Usage
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
            tool_calls,
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
                "streamed tool call at index={index} is missing an id"
            )));
        }
        if tool_call.name.is_empty() {
            return Err(AdapterError::Decode(format!(
                "streamed tool call at index={index} is missing a function name"
            )));
        }
    }
    Ok(tool_calls.into_values().collect())
}

//
pub fn decode_stream_response(
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
        'read: while let Some(chunk) = byte_stream.next().await {
            // A read interruption must not be unwrapped: it's a network error and should go
            // through the Transport classification so the upper layer decides on retry.
            let chunk = chunk.map_err(AdapterError::Transport)?;
            let text = String::from_utf8_lossy(&chunk);
            buffer.push_str(&text);

            while let Some(pos) = buffer.find("\n\n") {
                // I think this json structure should be the same, right?
                let section = buffer[..pos].to_string();
                // Remove the event just read and the two newlines
                buffer.drain(..pos + 2);

                let mut data = String::new();
                // Parse the data of each line
                for line in section.lines() {
                    if let Some(rest) = line.strip_prefix("data:") {
                        data.push_str(rest.trim());
                    }
                    // `event:` / `id:` currently have no consumer (the latter is mostly used for
                    // resume after disconnect), so ignore them rather than panic; keep last_event_id
                }
                // Why would this appear
                let payload = data.trim();
                if payload == "[DONE]" { break 'read; }
                if payload.is_empty() {
                    continue;
                }
                let value = serde_json::from_str::<dto::ModelStreamResponse>(data.trim()).map_err(|e| AdapterError::Decode(format!("failed to deserialize SSE payload: {e}")))?;
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

                if !reasoning_delta.is_empty() {
                    reasoning_content.push_str(&reasoning_delta);
                    yield AdapterEvent::ReasoningDelta(reasoning_delta);
                }
                if !content_delta.is_empty() {
                    content.push_str(&content_delta);
                    yield AdapterEvent::ContentDelta(content_delta);
                }
                // Save tool_calls
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
                // This always returns assistant, so next we yield the data outward
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProtocol;

    #[test]
    fn response_ids_can_be_omitted() {
        let stream: dto::ModelStreamResponse = serde_json::from_value(serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": null}]
        }))
        .expect("a streaming response should not require a top-level id");
        assert!(stream.id.is_empty());

        let response = decode_response(serde_json::json!({
            "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": "hi"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
        assert!(response.is_ok(), "a non-streaming response should not require a top-level id: {response:?}");
    }

    fn config() -> ModelConfig {
        ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost/v1/chat/completions")
            .model("test-model")
            .build()
    }

    fn request<'a>(
        messages: &'a [message::Message],
        tools: &'a [&'a tool::ToolDefinition],
    ) -> ModelRequest<'a> {
        ModelRequest { messages, tools, session_id: None }
    }

    fn body(config: &ModelConfig) -> Value {
        let empty: Vec<message::Message> = vec![];
        let tools: Vec<&tool::ToolDefinition> = vec![];
        encode_request(config, &request(&empty, &tools))
            .expect("encoding should succeed")
            .body
    }

    #[test]
    fn optional_fields_are_omitted_by_default() {
        let body = body(&config());
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("tool_choice").is_none());
        // thinking defaults to false → the field is not written (no `disabled` sent)
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn optional_fields_are_encoded_when_set() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost/v1/chat/completions")
            .model("test-model")
            .temperature(0.2)
            .max_output_tokens(1024)
            .reasoning_effort("low")
            .tool_choice(serde_json::json!("required"))
            .thinking(true)
            .build();

        let body = body(&config);
        assert_eq!(body["temperature"], serde_json::json!(0.2));
        assert_eq!(body["max_tokens"], serde_json::json!(1024));
        assert_eq!(body["reasoning_effort"], serde_json::json!("low"));
        assert_eq!(body["tool_choice"], serde_json::json!("required"));
        assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }));
    }

    #[test]
    fn extra_body_overrides_standard_fields() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost/v1/chat/completions")
            .model("test-model")
            .reasoning_effort("low")
            .extra_body(serde_json::json!({
                "reasoning_effort": "high",
                "enable_thinking": true,
            }))
            .build();

        let body = body(&config);
        assert_eq!(body["reasoning_effort"], serde_json::json!("high"));
        assert_eq!(body["enable_thinking"], serde_json::json!(true));
    }

    #[test]
    fn extra_body_null_deletes_key() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost/v1/chat/completions")
            .model("test-model")
            .thinking(true)
            .extra_body(serde_json::json!({ "thinking": null }))
            .build();

        let body = body(&config);
        assert!(
            body.get("thinking").is_none(),
            "null should delete the thinking key, actual: {body}"
        );
    }

    #[test]
    fn extra_body_ignores_non_object() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost/v1/chat/completions")
            .model("test-model")
            .extra_body(serde_json::json!("not an object"))
            .build();

        let body = body(&config);
        // A non-object is ignored; standard fields stay intact
        assert_eq!(body["model"], serde_json::json!("test-model"));
    }
}
