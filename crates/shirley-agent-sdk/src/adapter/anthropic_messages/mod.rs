//! Anthropic Messages protocol adapter (`POST /v1/messages`).
//!
//! References (from real captured traffic, see `docs/anthropic-messages-api.md`):
//! - endpoint `https://api.deepseek.com/anthropic/v1/messages`
//! - headers **`x-api-key`** + `anthropic-version` (not Bearer)
//!
//! Three critical differences from the other two protocols:
//! 1. **Tool results go in a `user` message** (Anthropic has no tool role), as a `tool_result` block;
//! 2. thinking is carried via `reasoning_content` (`signature` need not be echoed back; in practice it isn't validated);
//! 3. **`input_tokens` does not include cache reads** (opposite of OpenAI); add `cache_read` back when mapping.

mod dto;

use std::pin::Pin;

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;

use crate::{
    ModelConfig,
    adapter::{
        AdapterError, AdapterEvent, ModelFinishReason, ModelRequest, ModelResponse,
        PreparedRequest, sse,
    },
    message, tool,
};

/// Anthropic protocol version header, sent with every request.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Fallback value when `max_output_tokens` is not explicitly set.
///
/// The Anthropic spec requires `max_tokens` (the DeepSeek-compatible endpoint tolerates a
/// missing value, but real Anthropic does not), so provide a default for cross-provider portability.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Messages -> `(system, messages)`.
///
/// `System` / `ContextSummary` are lifted to the top-level `system`; the rest become content-block messages.
fn encode_messages(
    messages: &[message::Message],
) -> Result<(Option<String>, Vec<Value>), AdapterError> {
    let mut system: Vec<String> = Vec::new();
    let mut encoded: Vec<Value> = Vec::new();

    for message in messages {
        match message {
            message::Message::System { content } => system.push(content.clone()),
            message::Message::ContextSummary { content } => system.push(content.clone()),

            message::Message::User { content } => encoded.push(serde_json::json!({
                "role": "user",
                "content": super::content::anthropic_content(content)?,
            })),

            // Block order: thinking -> text -> tool_use. thinking must come first.
            message::Message::Assistant {
                content,
                reasoning_content,
                tool_calls,
            } => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(reasoning) = reasoning_content {
                    blocks.push(serde_json::json!({
                        "type": "thinking",
                        "thinking": reasoning,
                    }));
                }
                if let Some(content) = content
                    && !content.is_empty()
                {
                    blocks.push(serde_json::json!({ "type": "text", "text": content }));
                }
                for call in tool_calls {
                    // Internal arguments are a JSON string; Anthropic wants a JSON object.
                    let input: Value = serde_json::from_str(&call.arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": &call.id,
                        "name": &call.name,
                        "input": input,
                    }));
                }
                // An empty assistant (neither text nor tool calls) is not emitted, avoiding an empty content array.
                if !blocks.is_empty() {
                    encoded.push(serde_json::json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                }
            }

            // In Anthropic, a tool result is a `tool_result` block inside a **user message**.
            // A text-only result keeps its historical plain-string shape (byte-stable);
            // only a result that actually carries an image is encoded as content blocks
            // (Anthropic's `tool_result.content` natively accepts image blocks).
            message::Message::Tool {
                tool_call_id,
                content,
            } => {
                let block = match content {
                    Some(content) if content.has_images() => serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": super::content::anthropic_content(content)?,
                    }),
                    other => serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": other
                            .as_ref()
                            .map(|content| content.as_text())
                            .unwrap_or_default(),
                    }),
                };
                encoded.push(serde_json::json!({
                    "role": "user",
                    "content": [block],
                }));
            }
        }
    }

    let system = if system.is_empty() {
        None
    } else {
        Some(system.join("\n\n"))
    };
    Ok((system, encoded))
}

/// Tool definition -> Anthropic format (`input_schema` is just JSON Schema).
fn encode_tools(tools: &[&tool::ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": &tool.name,
                "description": &tool.description,
                "input_schema": &tool.parameters,
            })
        })
        .collect()
}

pub fn encode_request(
    config: &ModelConfig,
    input: &ModelRequest<'_>,
) -> Result<PreparedRequest, AdapterError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        "anthropic-version",
        HeaderValue::from_static(ANTHROPIC_VERSION),
    );
    if let Some(api_key) = &config.api_key {
        // Anthropic uses `x-api-key`, not `Authorization: Bearer`.
        let mut api_key_header = HeaderValue::from_str(api_key).map_err(|error| {
            AdapterError::Encode(format!("API key is not a valid header value: {error}"))
        })?;
        api_key_header.set_sensitive(true);
        headers.insert("x-api-key", api_key_header);
    }

    let (system, messages) = encode_messages(input.messages)?;
    let mut body = serde_json::json!({
        "model": &config.model,
        // Anthropic's `max_tokens` is required; use the fallback when unconfigured.
        "max_tokens": config.max_output_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
        "tools": encode_tools(input.tools),
        "stream": &config.stream,
    });

    if let Some(system) = system {
        body["system"] = serde_json::json!(system);
    }
    if let Some(temperature) = config.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    // In Anthropic, thinking is an object `{type:"enabled"}`; `budget_tokens` goes through extra_body.
    if config.thinking {
        body["thinking"] = serde_json::json!({ "type": "enabled" });
    }
    // Note: `reasoning_effort` has no counterpart in the Anthropic protocol and is **deliberately ignored**.
    if let Some(tool_choice) = &config.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }

    // The escape hatch is applied last (shallow merge, null deletes keys), consistent with the other two protocols.
    if let Some(extra) = &config.extra_body {
        apply_extra_body(&mut body, extra);
    }

    Ok(PreparedRequest {
        url: config.base_url.clone(),
        headers,
        body,
    })
}

/// Shallow-merge `extra` into the request body: app keys override standard keys, `null` deletes a
/// key, a non-object is ignored.
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

/// `content[]` -> internal assistant message.
///
/// Multiple blocks combine into one: thinking -> `reasoning_content`,
/// text -> `content` (concatenated), tool_use -> `ToolCall`.
fn decode_content(content: &[dto::ContentBlock]) -> message::Message {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut tool_calls = Vec::new();

    for block in content {
        match block {
            dto::ContentBlock::Text { text: chunk } => text.push_str(chunk),
            // signature is not echoed back (the server doesn't validate it in practice; see docs/anthropic-messages-api.md).
            dto::ContentBlock::Thinking {
                thinking: chunk, ..
            } => thinking.push_str(chunk),
            dto::ContentBlock::ToolUse { id, name, input } => {
                if id.is_empty() {
                    continue;
                }
                tool_calls.push(message::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    // input is an object -> the internal contract is a string.
                    arguments: input.to_string(),
                });
            }
            dto::ContentBlock::Other => {}
        }
    }

    message::Message::Assistant {
        content: (!text.is_empty()).then_some(text),
        reasoning_content: (!thinking.is_empty()).then_some(thinking),
        tool_calls,
    }
}

/// `stop_reason` → `ModelFinishReason`。
///
/// Prefer checking whether content contains `tool_use` (the same "don't read the string" principle
/// as Responses); `stop_reason` is auxiliary.
fn finish_reason(response: &dto::MessageResponse) -> ModelFinishReason {
    let has_tool_use = response
        .content
        .iter()
        .any(|block| matches!(block, dto::ContentBlock::ToolUse { .. }));
    if has_tool_use {
        return ModelFinishReason::ToolCalls;
    }
    match response.stop_reason.as_deref() {
        Some("max_tokens") => ModelFinishReason::Length,
        Some("refusal") => ModelFinishReason::Other("refusal".to_owned()),
        Some("pause_turn") => ModelFinishReason::Other("pause_turn".to_owned()),
        // end_turn / stop_sequence / None all count as normal completion.
        _ => ModelFinishReason::Stop,
    }
}

/// usage mapping. **Here `cache_read` must be added back to `input_tokens`** —
/// Anthropic's `input_tokens` does not include cache reads (opposite of OpenAI).
fn decode_usage(usage: Option<&dto::Usage>) -> message::Usage {
    let Some(usage) = usage else {
        return message::Usage::default();
    };
    let total_input = usage.input_tokens + usage.cache_read_input_tokens.unwrap_or(0);
    message::Usage {
        input_tokens: total_input,
        output_tokens: usage.output_tokens,
        // Hit = cache read; only Some when the server reported the field (otherwise None, not 0).
        cached_input_tokens: usage.cache_read_input_tokens,
        cache_reported_input_tokens: usage.cache_read_input_tokens.map(|_| total_input),
        // Anthropic doesn't report reasoning tokens separately (thinking is counted in output_tokens).
        reasoning_tokens: None,
    }
}

pub fn decode_response(body: Value) -> Result<ModelResponse, AdapterError> {
    let response = serde_json::from_value::<dto::MessageResponse>(body)
        .map_err(|error| AdapterError::Decode(format!("failed to parse response: {error}")))?;
    Ok(ModelResponse {
        message: decode_content(&response.content),
        finish_reason: finish_reason(&response),
        usage: decode_usage(response.usage.as_ref()),
    })
}

/// Streaming decode core. Split from the network layer for testability.
pub fn decode_stream_bytes<S>(
    byte_stream: S,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    Box::pin(async_stream::try_stream! {
        futures::pin_mut!(byte_stream);
        let mut buffer = String::new();

        // In-stream aggregation state.
        let mut text = String::new();
        let mut thinking = String::new();
        // Aggregate tool arguments by content block `index` (concatenating input_json_delta fragments).
        let mut tools: Vec<(usize, String, String, String)> = Vec::new(); // (index, id, name, args)
        // usage is given in two places: message_start for the input side, message_delta for the output side.
        let mut input_usage: Option<dto::Usage> = None;
        let mut output_usage: Option<dto::Usage> = None;
        let mut stop_reason: Option<String> = None;

        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.map_err(AdapterError::Transport)?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            for section in sse::drain_sections(&mut buffer) {
                let Some(data) = sse::data_payload(&section) else {
                    continue;
                };
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let event = serde_json::from_str::<dto::StreamEvent>(&data).map_err(|error| {
                    AdapterError::Decode(format!("failed to deserialize SSE payload: {error}"))
                })?;

                match event {
                    dto::StreamEvent::MessageStart { message } => {
                        input_usage = message.usage;
                    }
                    dto::StreamEvent::ContentBlockStart { index, content_block } => {
                        if let dto::ContentBlock::ToolUse { id, name, .. } = content_block {
                            tools.push((index, id, name, String::new()));
                        }
                    }
                    dto::StreamEvent::ContentBlockDelta { index, delta } => match delta {
                        dto::Delta::Text { text: chunk } => {
                            text.push_str(&chunk);
                            yield AdapterEvent::ContentDelta(chunk);
                        }
                        dto::Delta::Thinking { thinking: chunk } => {
                            thinking.push_str(&chunk);
                            yield AdapterEvent::ReasoningDelta(chunk);
                        }
                        // signature is not echoed back; ignore it (the server doesn't validate it in practice).
                        dto::Delta::Signature { .. } => {}
                        dto::Delta::InputJson { partial_json } => {
                            if let Some(entry) = tools.iter_mut().find(|(i, ..)| *i == index) {
                                entry.3.push_str(&partial_json);
                            }
                        }
                        dto::Delta::Other => {}
                    },
                    dto::StreamEvent::ContentBlockStop { .. } => {}
                    dto::StreamEvent::MessageDelta { delta, usage } => {
                        stop_reason = delta.stop_reason;
                        output_usage = usage;
                    }
                    dto::StreamEvent::MessageStop => {
                        let response = assemble_stream_response(
                            &text,
                            &thinking,
                            &tools,
                            stop_reason.clone(),
                            input_usage.as_ref(),
                            output_usage.as_ref(),
                        );
                        yield AdapterEvent::Finished(response);
                        return;
                    }
                    dto::StreamEvent::Other => {}
                }
            }
        }
    })
}

/// Assemble the in-stream aggregation state into the final response.
#[allow(clippy::too_many_arguments)]
fn assemble_stream_response(
    text: &str,
    thinking: &str,
    tools: &[(usize, String, String, String)],
    stop_reason: Option<String>,
    input_usage: Option<&dto::Usage>,
    output_usage: Option<&dto::Usage>,
) -> ModelResponse {
    let tool_calls: Vec<message::ToolCall> = tools
        .iter()
        .map(|(_, id, name, args)| message::ToolCall {
            id: id.clone(),
            name: name.clone(),
            arguments: args.clone(),
        })
        .collect();

    let assistant = message::Message::Assistant {
        content: (!text.is_empty()).then(|| text.to_owned()),
        reasoning_content: (!thinking.is_empty()).then(|| thinking.to_owned()),
        tool_calls,
    };

    // Merge the two usage sources: input side from message_start, output side from message_delta.
    let merged = merge_stream_usage(input_usage, output_usage);

    // finish_reason reuses the same determination: tool_use present means ToolCalls.
    let has_tool_use = matches!(
        &assistant,
        message::Message::Assistant { tool_calls, .. } if !tool_calls.is_empty()
    );
    let finish = if has_tool_use {
        ModelFinishReason::ToolCalls
    } else {
        match stop_reason.as_deref() {
            Some("max_tokens") => ModelFinishReason::Length,
            Some("refusal") => ModelFinishReason::Other("refusal".to_owned()),
            Some("pause_turn") => ModelFinishReason::Other("pause_turn".to_owned()),
            _ => ModelFinishReason::Stop,
        }
    };

    ModelResponse {
        message: assistant,
        finish_reason: finish,
        usage: merged,
    }
}

/// Merge the usage reported in two places in the stream.
fn merge_stream_usage(
    input_usage: Option<&dto::Usage>,
    output_usage: Option<&dto::Usage>,
) -> message::Usage {
    let Some(input) = input_usage else {
        // With no input side, fall back to the output side (at least get output_tokens).
        return decode_usage(output_usage);
    };
    let output_tokens = output_usage.map(|u| u.output_tokens).unwrap_or(0);
    let total_input = input.input_tokens + input.cache_read_input_tokens.unwrap_or(0);
    message::Usage {
        input_tokens: total_input,
        output_tokens,
        cached_input_tokens: input.cache_read_input_tokens,
        cache_reported_input_tokens: input.cache_read_input_tokens.map(|_| total_input),
        reasoning_tokens: None,
    }
}

pub fn decode_stream_response(
    response: reqwest::Response,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>> {
    decode_stream_bytes(response.bytes_stream())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProtocol;

    fn config() -> ModelConfig {
        ModelConfig::builder()
            .protocol(ModelProtocol::AnthropicMessages)
            .base_url("http://localhost/v1/messages")
            .model("deepseek-flash")
            .build()
    }

    fn request<'a>(
        messages: &'a [message::Message],
        tools: &'a [&'a tool::ToolDefinition],
    ) -> ModelRequest<'a> {
        ModelRequest {
            messages,
            tools,
            session_id: None,
        }
    }

    fn body(config: &ModelConfig, messages: &[message::Message]) -> Value {
        let tools: Vec<&tool::ToolDefinition> = vec![];
        encode_request(config, &request(messages, &tools))
            .expect("encoding should succeed")
            .body
    }

    fn sample_tool() -> tool::ToolDefinition {
        tool::ToolDefinition {
            name: "bash".into(),
            description: "run a command".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
            }),
        }
    }

    #[test]
    fn api_key_goes_to_x_api_key_header_with_version() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::AnthropicMessages)
            .base_url("http://localhost/v1/messages")
            .model("deepseek-flash")
            .api_key("sk-secret")
            .build();
        let tools: Vec<&tool::ToolDefinition> = vec![];
        let prepared =
            encode_request(&config, &request(&[], &tools)).expect("encoding should succeed");
        assert_eq!(prepared.headers.get("x-api-key").unwrap(), "sk-secret");
        assert!(prepared.headers.get("authorization").is_none());
        assert_eq!(
            prepared.headers.get("anthropic-version").unwrap(),
            ANTHROPIC_VERSION
        );
    }

    #[test]
    fn system_goes_to_top_level_system_not_messages() {
        let messages = vec![
            message::Message::System {
                content: "You are an assistant".into(),
            },
            message::Message::ContextSummary {
                content: "summary".into(),
            },
            message::Message::User {
                content: "hello".into(),
            },
        ];
        let body = body(&config(), &messages);
        assert_eq!(
            body["system"],
            serde_json::json!("You are an assistant\n\nsummary")
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "system/summary should not enter messages"
        );
        assert_eq!(messages[0]["role"], serde_json::json!("user"));
    }

    #[test]
    fn user_message_uses_text_block() {
        let messages = vec![message::Message::User {
            content: "hello".into(),
        }];
        let body = body(&config(), &messages);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"][0]["type"], serde_json::json!("text"));
        assert_eq!(
            messages[0]["content"][0]["text"],
            serde_json::json!("hello")
        );
    }

    #[test]
    fn assistant_block_order_is_thinking_text_tool_use() {
        let messages = vec![message::Message::Assistant {
            content: Some("let me check".into()),
            reasoning_content: Some("think first".into()),
            tool_calls: vec![message::ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                arguments: "{\"command\":\"ls\"}".into(),
            }],
        }];
        let body = body(&config(), &messages);
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], serde_json::json!("thinking"));
        assert_eq!(blocks[0]["thinking"], serde_json::json!("think first"));
        // signature is not echoed back (the server doesn't validate it in practice); it should not appear in the encoding.
        assert!(blocks[0].get("signature").is_none());
        assert_eq!(blocks[1]["type"], serde_json::json!("text"));
        assert_eq!(blocks[2]["type"], serde_json::json!("tool_use"));
        // input must be a JSON object, not a string.
        assert_eq!(blocks[2]["input"], serde_json::json!({"command": "ls"}));
    }

    #[test]
    fn tool_message_becomes_user_tool_result_block() {
        let messages = vec![message::Message::Tool {
            tool_call_id: "call_1".into(),
            content: Some("Sunny, 25C".into()),
        }];
        let body = body(&config(), &messages);
        let messages = body["messages"].as_array().unwrap();
        // In Anthropic, a tool result is a user message, not a tool role.
        assert_eq!(messages[0]["role"], serde_json::json!("user"));
        assert_eq!(
            messages[0]["content"][0]["type"],
            serde_json::json!("tool_result")
        );
        assert_eq!(
            messages[0]["content"][0]["tool_use_id"],
            serde_json::json!("call_1")
        );
        assert_eq!(
            messages[0]["content"][0]["content"],
            serde_json::json!("Sunny, 25C")
        );
    }

    #[test]
    fn tool_schema_uses_input_schema_key() {
        let tool = sample_tool();
        let tools = vec![&tool];
        let prepared = encode_request(&config(), &request(&[], &tools)).unwrap();
        let encoded = &prepared.body["tools"][0];
        assert_eq!(encoded["name"], serde_json::json!("bash"));
        assert_eq!(encoded["description"], serde_json::json!("run a command"));
        // The key name is input_schema; the value is just JSON Schema.
        assert_eq!(encoded["input_schema"]["type"], serde_json::json!("object"));
        assert!(encoded.get("parameters").is_none());
    }

    #[test]
    fn thinking_is_object_not_string() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::AnthropicMessages)
            .base_url("http://localhost/v1/messages")
            .model("deepseek-flash")
            .thinking(true)
            .build();
        let body = body(&config, &[]);
        assert_eq!(body["thinking"], serde_json::json!({"type": "enabled"}));
    }

    #[test]
    fn max_tokens_defaults_when_absent() {
        let body = body(&config(), &[]);
        assert_eq!(body["max_tokens"], serde_json::json!(DEFAULT_MAX_TOKENS));
    }

    #[test]
    fn max_tokens_honors_config() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::AnthropicMessages)
            .base_url("http://localhost/v1/messages")
            .model("deepseek-flash")
            .max_output_tokens(1234)
            .build();
        let body = body(&config, &[]);
        assert_eq!(body["max_tokens"], serde_json::json!(1234));
    }

    #[test]
    fn reasoning_effort_is_ignored() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::AnthropicMessages)
            .base_url("http://localhost/v1/messages")
            .model("deepseek-flash")
            .reasoning_effort("high")
            .build();
        let body = body(&config, &[]);
        assert!(
            body.get("reasoning_effort").is_none(),
            "Anthropic has no such field; it should be ignored"
        );
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn extra_body_overrides_and_null_deletes() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::AnthropicMessages)
            .base_url("http://localhost/v1/messages")
            .model("deepseek-flash")
            .extra_body(serde_json::json!({
                "thinking": { "type": "enabled", "budget_tokens": 2048 },
                "system": null
            }))
            .build();
        let body = body(
            &config,
            &[message::Message::System {
                content: "s".into(),
            }],
        );
        assert_eq!(body["thinking"]["budget_tokens"], serde_json::json!(2048));
        assert!(body.get("system").is_none(), "null should delete the key");
    }

    #[test]
    fn decode_content_blocks_into_single_assistant() {
        let body = serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "think first", "signature": "sig-1" },
                { "type": "text", "text": "answer" },
                { "type": "tool_use", "id": "call_1", "name": "bash", "input": { "command": "ls" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        });
        let response = decode_response(body).unwrap();
        match response.message {
            message::Message::Assistant {
                content,
                reasoning_content,
                tool_calls,
                ..
            } => {
                assert_eq!(content.as_deref(), Some("answer"));
                assert_eq!(reasoning_content.as_deref(), Some("think first"));
                assert_eq!(tool_calls.len(), 1);
                // input object -> arguments string.
                assert_eq!(tool_calls[0].arguments, "{\"command\":\"ls\"}");
            }
            other => panic!("expected assistant, got {other:?}"),
        }
        assert_eq!(response.finish_reason, ModelFinishReason::ToolCalls);
    }

    #[test]
    fn decode_usage_adds_cache_read_back() {
        // Real captured traffic: input=134, cache_read=2301 -> total input 2435.
        let body = serde_json::json!({
            "content": [{ "type": "text", "text": "hi" }],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 134,
                "output_tokens": 12,
                "cache_read_input_tokens": 2301,
                "cache_creation_input_tokens": 0
            }
        });
        let response = decode_response(body).unwrap();
        assert_eq!(
            response.usage.input_tokens, 2435,
            "input must add cache_read back"
        );
        assert_eq!(response.usage.cached_input_tokens, Some(2301));
        assert_eq!(response.usage.cache_reported_input_tokens, Some(2435));
        let rate = response.usage.cache_hit_rate().unwrap();
        assert!(
            (rate - 2301.0 / 2435.0).abs() < 1e-9,
            "the hit rate should be within (0,1)"
        );
    }

    #[test]
    fn decode_usage_absent_cache_is_none_not_zero() {
        let body = serde_json::json!({
            "content": [{ "type": "text", "text": "hi" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 10, "output_tokens": 3 }
        });
        let response = decode_response(body).unwrap();
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(
            response.usage.cached_input_tokens, None,
            "unreported should be None"
        );
        assert_eq!(response.usage.cache_hit_rate(), None);
    }

    #[test]
    fn finish_reason_maps_stop_reasons() {
        let cases = [
            ("end_turn", ModelFinishReason::Stop),
            ("stop_sequence", ModelFinishReason::Stop),
            ("max_tokens", ModelFinishReason::Length),
        ];
        for (stop_reason, expected) in cases {
            let body = serde_json::json!({
                "content": [{ "type": "text", "text": "x" }],
                "stop_reason": stop_reason,
            });
            let response = decode_response(body).unwrap();
            assert_eq!(
                response.finish_reason, expected,
                "stop_reason={stop_reason}"
            );
        }
    }

    #[test]
    fn finish_reason_prefers_tool_use_over_stop_reason() {
        let body = serde_json::json!({
            "content": [{ "type": "tool_use", "id": "c1", "name": "bash", "input": {} }],
            "stop_reason": "end_turn",
        });
        let response = decode_response(body).unwrap();
        assert_eq!(response.finish_reason, ModelFinishReason::ToolCalls);
    }

    /// Feed a sequence of SSE texts to the streaming decode core and collect the events.
    async fn collect_stream(chunks: Vec<&'static str>) -> Vec<AdapterEvent> {
        let stream = futures::stream::iter(
            chunks
                .into_iter()
                .map(|c| Ok::<_, reqwest::Error>(bytes::Bytes::from_static(c.as_bytes())))
                .collect::<Vec<_>>(),
        );
        let mut events = Vec::new();
        let mut decoded = decode_stream_bytes(stream);
        while let Some(event) = decoded.next().await {
            events.push(event.expect("streaming decode should not error"));
        }
        events
    }

    #[tokio::test]
    async fn stream_yields_deltas_and_finishes_without_done_marker() {
        // Key point: no `data: [DONE]`; it ends via message_stop.
        let chunks = vec![
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":4}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"think\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-1\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        let events = collect_stream(chunks).await;

        assert_eq!(events.len(), 3, "thinking delta + text delta + Finished");
        assert!(matches!(&events[0], AdapterEvent::ReasoningDelta(d) if d == "think"));
        assert!(matches!(&events[1], AdapterEvent::ContentDelta(d) if d == "Hello"));
        match &events[2] {
            AdapterEvent::Finished(response) => {
                assert_eq!(response.finish_reason, ModelFinishReason::Stop);
                match &response.message {
                    message::Message::Assistant {
                        content,
                        reasoning_content,
                        ..
                    } => {
                        assert_eq!(content.as_deref(), Some("Hello"));
                        assert_eq!(reasoning_content.as_deref(), Some("think"));
                    }
                    other => panic!("expected assistant, actual: {other:?}"),
                }
                // usage is given in two places: input side + output side; they must be merged.
                assert_eq!(response.usage.input_tokens, 14, "10 + cache_read 4");
                assert_eq!(response.usage.cached_input_tokens, Some(4));
                assert_eq!(response.usage.output_tokens, 2);
            }
            other => panic!("the last event should be Finished, actual: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_aggregates_input_json_delta_fragments() {
        // Anthropic's tool_use.input can only be assembled from input_json_delta fragments.
        let chunks = vec![
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"bash\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\":\\\"ls\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        let events = collect_stream(chunks).await;
        assert_eq!(events.len(), 1, "only one Finished (no delta text)");
        match &events[0] {
            AdapterEvent::Finished(response) => {
                assert_eq!(response.finish_reason, ModelFinishReason::ToolCalls);
                match &response.message {
                    message::Message::Assistant { tool_calls, .. } => {
                        assert_eq!(tool_calls.len(), 1);
                        assert_eq!(tool_calls[0].id, "call_1");
                        assert_eq!(tool_calls[0].name, "bash");
                        assert_eq!(tool_calls[0].arguments, "{\"command\":\"ls\"}");
                    }
                    other => panic!("expected an assistant with tool calls, actual: {other:?}"),
                }
            }
            other => panic!("expected Finished, actual: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_handles_split_chunks() {
        // An event split across two chunks; framing must reassemble it correctly.
        let chunks = vec![
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_del",
            "ta\",\"text\":\"part\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        let events = collect_stream(chunks).await;
        assert!(matches!(&events[0], AdapterEvent::ContentDelta(d) if d == "part"));
        assert!(matches!(&events[1], AdapterEvent::Finished(_)));
    }

    /// Real-network end-to-end test (ignored by default).
    ///
    /// Requires environment variables:
    /// - `DEEPSEEK_API_KEY` (required; if missing, skip and return)
    /// - `SHIRLEY_ANTHROPIC_BASE_URL` (optional, default `https://api.deepseek.com/anthropic/v1/messages`)
    /// - `SHIRLEY_ANTHROPIC_MODEL` (optional, default `deepseek-flash`)
    ///
    /// How to run:
    /// ```sh
    /// cargo test -p shirley-agent-sdk --lib anthropic_messages::tests::live -- --ignored --nocapture
    /// ```
    fn live_config(stream: bool) -> Option<ModelConfig> {
        let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
            eprintln!("skipping: DEEPSEEK_API_KEY not set");
            return None;
        };
        let base_url = std::env::var("SHIRLEY_ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/anthropic/v1/messages".to_owned());
        let model = std::env::var("SHIRLEY_ANTHROPIC_MODEL")
            .unwrap_or_else(|_| "deepseek-flash".to_owned());
        Some(
            ModelConfig::builder()
                .protocol(ModelProtocol::AnthropicMessages)
                .base_url(base_url)
                .model(model)
                .api_key(api_key)
                .stream(stream)
                .build(),
        )
    }

    async fn run_once(
        client: &reqwest::Client,
        config: &ModelConfig,
        messages: &[message::Message],
        tools: &[&tool::ToolDefinition],
    ) -> ModelResponse {
        let request = ModelRequest {
            messages,
            tools,
            session_id: None,
        };
        let mut stream = crate::adapter::invoke(client, config, request).await;
        while let Some(event) = stream.next().await {
            if let AdapterEvent::Finished(response) = event.expect("the request should not error") {
                return response;
            }
        }
        panic!("the stream should produce Finished");
    }

    #[tokio::test]
    #[ignore = "real network call, requires DEEPSEEK_API_KEY"]
    async fn live_anthropic_round_trip() {
        let Some(config) = live_config(true) else {
            return;
        };
        let messages = vec![
            message::Message::System {
                content: "You are a helpful assistant.".into(),
            },
            message::Message::User {
                content: "Say hi in exactly one word.".into(),
            },
        ];
        let tools: Vec<&tool::ToolDefinition> = vec![];
        let request = ModelRequest {
            messages: &messages,
            tools: &tools,
            session_id: None,
        };
        let client = reqwest::Client::new();
        use futures::StreamExt;
        let mut stream = crate::adapter::invoke(&client, &config, request).await;

        let mut content = String::new();
        let mut finished = None;
        while let Some(event) = stream.next().await {
            match event.expect("streaming events should not error") {
                AdapterEvent::Retrying { .. } => {}
                AdapterEvent::ImagesDropped { .. } => {}
                AdapterEvent::ContentDelta(delta) => content.push_str(&delta),
                AdapterEvent::ReasoningDelta(_) => {}
                AdapterEvent::Finished(response) => {
                    finished = Some(response);
                    break;
                }
            }
        }
        let finished = finished.expect("the stream should produce Finished");
        eprintln!("content = {content:?}");
        eprintln!("finish  = {:?}", finished.finish_reason);
        eprintln!("usage   = {:?}", finished.usage);
        assert!(
            !content.trim().is_empty(),
            "should get non-empty text output"
        );
        assert!(
            finished.usage.input_tokens > 0,
            "usage.input_tokens should be > 0"
        );
    }

    /// Real-network **tool round-trip** test (ignored by default), verifying the most critical path:
    /// the model emits a `tool_use` on the first turn (possibly with thinking),
    /// we send back the assistant (with the thinking block) and a `tool_result`,
    /// and the second turn gets the final text.
    ///
    /// This locks in: `tool_result` must go in a user message (Anthropic has no tool role).
    #[tokio::test]
    #[ignore = "real network call, requires DEEPSEEK_API_KEY"]
    async fn live_anthropic_tool_round_trip() {
        let Some(config) = live_config(false) else {
            return;
        };
        let tool_definition = tool::ToolDefinition {
            name: "get_weather".into(),
            description: "look up city weather".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        };
        let tools = vec![&tool_definition];
        let client = reqwest::Client::new();

        let first_messages = vec![
            message::Message::System {
                content: "You are an assistant.".into(),
            },
            message::Message::User {
                content: "What's the weather in Beijing now? Look it up with the tool.".into(),
            },
        ];
        let first = run_once(&client, &config, &first_messages, &tools).await;
        let call_id = match &first.message {
            message::Message::Assistant { tool_calls, .. } => {
                let call = tool_calls.first().expect("should emit one tool call");
                eprintln!("tool_call = {} / {}", call.id, call.name);
                assert_eq!(call.name, "get_weather");
                assert!(
                    call.arguments.contains("city"),
                    "arguments should contain city"
                );
                call.id.clone()
            }
            other => {
                panic!("the first turn should be an assistant with tool calls, actual: {other:?}")
            }
        };
        assert_eq!(first.finish_reason, ModelFinishReason::ToolCalls);

        // Second turn: send back first.message (with thinking) + tool_result.
        let second_messages = vec![
            message::Message::System {
                content: "You are an assistant.".into(),
            },
            message::Message::User {
                content: "What's the weather in Beijing now? Look it up with the tool.".into(),
            },
            first.message.clone(),
            message::Message::Tool {
                tool_call_id: call_id,
                content: Some("Sunny, 25C".into()),
            },
        ];
        let second = run_once(&client, &config, &second_messages, &tools).await;
        match &second.message {
            message::Message::Assistant { content, .. } => {
                let text = content.clone().unwrap_or_default();
                eprintln!("final = {text:?}");
                assert!(
                    !text.trim().is_empty(),
                    "the second turn should give final text"
                );
            }
            other => panic!("the second turn should be a text assistant, actual: {other:?}"),
        }
        assert_eq!(second.finish_reason, ModelFinishReason::Stop);
    }

    /// Real-network **streaming tool call** test (ignored by default).
    ///
    /// Unlike Responses: the terminal state does **not** carry the full input; arguments must be
    /// assembled from `input_json_delta` fragments. This locks in that aggregation logic.
    #[tokio::test]
    #[ignore = "real network call, requires DEEPSEEK_API_KEY"]
    async fn live_anthropic_streaming_tool_call() {
        let Some(config) = live_config(true) else {
            return;
        };
        let tool_definition = tool::ToolDefinition {
            name: "get_weather".into(),
            description: "look up city weather".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        };
        let tools = vec![&tool_definition];
        let messages = vec![
            message::Message::System {
                content: "You are an assistant.".into(),
            },
            message::Message::User {
                content: "Beijing weather? Look it up with the tool.".into(),
            },
        ];
        let request = ModelRequest {
            messages: &messages,
            tools: &tools,
            session_id: None,
        };
        use futures::StreamExt;
        let client = reqwest::Client::new();
        let mut stream = crate::adapter::invoke(&client, &config, request).await;
        let mut reasoning = String::new();
        let mut finished = None;
        while let Some(event) = stream.next().await {
            match event.expect("streaming events should not error") {
                AdapterEvent::Retrying { .. } => {}
                AdapterEvent::ImagesDropped { .. } => {}
                AdapterEvent::ReasoningDelta(delta) => reasoning.push_str(&delta),
                AdapterEvent::ContentDelta(_) => {}
                AdapterEvent::Finished(response) => {
                    finished = Some(response);
                    break;
                }
            }
        }
        let finished = finished.expect("the stream should produce Finished");
        eprintln!("reasoning = {} chars", reasoning.len());
        match &finished.message {
            message::Message::Assistant { tool_calls, .. } => {
                let call = tool_calls
                    .first()
                    .expect("the terminal state should contain a tool_use");
                eprintln!(
                    "tool_call = {} / {} / {}",
                    call.id, call.name, call.arguments
                );
                assert_eq!(call.name, "get_weather");
                assert!(!call.id.is_empty(), "tool_use id should not be empty");
                assert!(
                    call.arguments.contains("city"),
                    "arguments should be the fully aggregated JSON"
                );
            }
            other => panic!("expected an assistant with tool calls, actual: {other:?}"),
        }
        assert_eq!(finished.finish_reason, ModelFinishReason::ToolCalls);
    }
}
