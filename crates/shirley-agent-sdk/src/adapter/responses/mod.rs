//! Responses protocol adapter (`POST /responses`).
//!
//! References:
//! - <https://api-docs.deepseek.com/guides/responses_api/>
//! - <https://api-docs.deepseek.com/zh-cn/api/create-response>
//!
//! Differences from `ChatCompletions` (see `docs/responses-api.md`):
//! - system stays in place in `input` as a normal `{role:"system"}` item (no top-level
//!   `instructions`: the server inserts those before `input`, breaking prefix caching);
//! - `input` is an **item list**: an assistant's text and the `function_call` it emitted are
//!   **sibling items** (one-to-many expansion); tool results go through `function_call_output`;
//! - tool definitions lack one level of `function` wrapping;
//! - streaming has **no `data: [DONE]`**; it ends with `response.completed` / `incomplete` /
//!   `failed`.

mod dto;

use std::pin::Pin;

use futures::StreamExt;
use reqwest::header::HeaderValue;
use serde_json::Value;

use crate::{
    ModelConfig,
    adapter::{
        AdapterError, AdapterEvent, ModelFinishReason, ModelRequest, ModelResponse,
        PreparedRequest, sse,
    },
    message, tool,
};

/// Messages -> `input` item list.
///
/// **Pure order-preserving mapping**: the internal `Message` list becomes items one by one in
/// original order, never reordered. This is the adapter layer's only outward promise — the upper
/// layer need not know where system goes or whether to hoist it; all those protocol details are
/// contained here.
///
/// Order preservation is the structural guarantee for **KV prefix caching**: appending a message
/// = adding an item at the end, without touching a single byte before it, so the prefix hits
/// naturally. **Do not introduce any sort / grouping / position adjustment here.**
///
/// `System` / `ContextSummary` stay in place as normal `{role:"system"}` items (the schema's
/// `EasyInputMessage.role` includes `system`, and any position in `input` is legal); **no
/// top-level `instructions`**: the server always inserts those before `input`, which moves a
/// mid-list / trailing system to the front and breaks the prefix.
///
/// **One-to-many**: an assistant with text + `tool_calls` produces "a text item + several
/// `function_call` items".
fn encode_input(messages: &[message::Message]) -> Vec<Value> {
    let mut input: Vec<Value> = Vec::new();

    for message in messages {
        match message {
            // system-level content stays in place as a system item, not hoisted or merged.
            message::Message::System { content } | message::Message::ContextSummary { content } => {
                input.push(serde_json::json!({
                    "type": "message",
                    "role": "system",
                    "content": content,
                }));
            }

            message::Message::User { content } => input.push(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": super::content::responses_content(content),
            })),

            // Historical reasoning (reasoning_content) is deliberately not echoed back: the server
            // accepts reasoning items, but feeding old reasoning back has no benefit; use
            // extra_body if needed.
            message::Message::Assistant {
                content,
                reasoning_content: _,
                tool_calls,
            } => {
                // Text and tool calls are sibling items, pushed separately.
                //
                // Historical assistant text uses **plain string** content: OpenAI's
                // `EasyInputMessage` only accepts `string | input_text/input_image/input_file`
                // blocks, and `output_text` belongs only to **output** items (`OutputMessage`
                // also needs `id`/`status`). A string is the most strictly compliant and simplest
                // form.
                if let Some(content) = content
                    && !content.is_empty()
                {
                    input.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": content,
                    }));
                }

                for call in tool_calls {
                    input.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": &call.id,
                        "name": &call.name,
                        "arguments": &call.arguments,
                    }));
                }
            }

            message::Message::Tool {
                tool_call_id,
                content,
            } => {
                // `function_call_output` is text-only; an image result (a computer-use
                // screenshot) is split into the text output plus a trailing user item
                // that carries the image.
                input.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    // output allows a string or an array of content blocks; a string is most direct for plain text.
                    "output": content
                        .as_ref()
                        .map(super::super::message::Content::as_text)
                        .unwrap_or_default(),
                }));
                if let Some(images) = content.as_ref().and_then(super::content::images_only) {
                    input.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": super::content::responses_content(&images),
                    }));
                }
            }
        }
    }

    input
}

/// Tool definition -> Responses format (one less level of `function` wrapping).
fn encode_tools(tools: &[&tool::ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": &tool.name,
                "description": &tool.description,
                "parameters": &tool.parameters,
            })
        })
        .collect()
}

pub fn encode_request(
    config: &ModelConfig,
    input: &ModelRequest<'_>,
) -> Result<PreparedRequest, AdapterError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    if let Some(api_key) = &config.api_key {
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|error| {
                AdapterError::Encode(format!("API key is not a valid header value: {error}"))
            })?;
        authorization.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
    }
    super::attach_local_session_header(&mut headers, &config.base_url, input.session_id)?;

    let items = encode_input(input.messages);
    let mut body = serde_json::json!({
        "model": &config.model,
        "input": items,
        "tools": encode_tools(input.tools),
        "stream": &config.stream,
    });

    if let Some(temperature) = config.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    // The key name in Responses is `max_output_tokens` (ChatCompletions uses `max_tokens`).
    if let Some(max_output_tokens) = config.max_output_tokens {
        body["max_output_tokens"] = serde_json::json!(max_output_tokens);
    }
    // reasoning_effort here must be wrapped into a nested object `{"reasoning": {"effort": ...}}`,
    // unlike ChatCompletions' top-level string shape.
    if let Some(reasoning_effort) = &config.reasoning_effort {
        body["reasoning"] = serde_json::json!({ "effort": reasoning_effort });
    }
    if let Some(tool_choice) = &config.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }

    // The escape hatch is applied last, with the same semantics as ChatCompletions (shallow merge, null deletes keys).
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

/// Flatten `output[]` into one internal assistant message.
///
/// Many-to-one: the three item kinds reasoning / message / `function_call` combine into
/// `Message::Assistant { content, reasoning_content, tool_calls }`.
fn decode_output(response: &dto::Response) -> ModelResponse {
    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls = Vec::new();

    for item in &response.output {
        match item {
            dto::OutputItem::Message(message) => {
                if let Some(part) = &message.content {
                    content.push_str(&part.text());
                }
            }
            dto::OutputItem::Reasoning(reasoning) => {
                // OpenAI-family reasoning models return only `summary` by default (`content` is the
                // raw CoT, usually empty); implementations like DeepSeek use `content`. Take the
                // non-empty one, each as the other's fallback.
                let text = match (
                    reasoning.summary.as_ref().map(dto::Content::text),
                    reasoning.content.as_ref().map(dto::Content::text),
                ) {
                    (Some(summary), _) if !summary.is_empty() => summary,
                    (_, Some(content)) => content,
                    (Some(summary), None) => summary,
                    (None, None) => String::new(),
                };
                reasoning_content.push_str(&text);
            }
            dto::OutputItem::FunctionCall(call) => {
                let id = call.call_id.clone().unwrap_or_default();
                if id.is_empty() {
                    // A missing call_id can't pair with a tool result, so letting the upper layer
                    // error is safer; but stay lenient here: an empty id is rejected at tool
                    // execution time.
                    continue;
                }
                tool_calls.push(message::ToolCall {
                    id,
                    name: call.name.clone().unwrap_or_default(),
                    arguments: call.arguments.clone(),
                });
            }
            dto::OutputItem::Unknown => {}
        }
    }

    let finish_reason = finish_reason(response, !tool_calls.is_empty());

    ModelResponse {
        message: message::Message::Assistant {
            content: (!content.is_empty()).then_some(content),
            reasoning_content: (!reasoning_content.is_empty()).then_some(reasoning_content),
            tool_calls,
        },
        finish_reason,
        usage: decode_usage(response.usage.as_ref()),
    }
}

/// Streaming terminal decode: take `response.output` as authoritative, **backfilling the
/// accumulated delta when reasoning is missing**.
///
/// Why backfill: Responses' streaming `response.completed` should carry the full `output`
/// (including the reasoning item), but in practice the `DeepSeek` compatibility layer returns only
/// the message item, with reasoning appearing only as deltas. Without backfilling,
/// `Message::Assistant.reasoning_content` would always be `None`, and on the TUI the thinking
/// content would flash once and be wiped by the terminal message.
///
/// Contract: when `output` **has** a reasoning item, it wins (complete, authoritative); only when
/// absent does the accumulated value fill in — the two never overwrite or duplicate each other.
fn decode_output_with_reasoning(
    response: &dto::Response,
    accumulated_reasoning: &str,
) -> ModelResponse {
    let mut decoded = decode_output(response);
    if let message::Message::Assistant {
        reasoning_content, ..
    } = &mut decoded.message
        && reasoning_content.is_none()
        && !accumulated_reasoning.is_empty()
    {
        *reasoning_content = Some(accumulated_reasoning.to_owned());
    }
    decoded
}

/// `finish_reason` determination: Responses **has no** `finish_reason` field; tool calls must be
/// detected by whether `output` contains a `function_call`.
fn finish_reason(response: &dto::Response, has_tool_calls: bool) -> ModelFinishReason {
    match response.status.as_deref() {
        Some("incomplete") => match response
            .incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref())
        {
            // Hit max_output_tokens (could also be content_filter; both go to Other, not Length)
            Some("max_output_tokens") => ModelFinishReason::Length,
            Some(other) => ModelFinishReason::Other(other.to_owned()),
            None => ModelFinishReason::Other("incomplete".to_owned()),
        },
        // `failed` is already turned into an error in `decode_response` / the streaming `Failed`
        // branch and won't reach here; should it arrive (e.g. the upstream missed it), fall back
        // to Other rather than falsely reporting Stop.
        Some("failed") => ModelFinishReason::Other("failed".to_owned()),
        // `cancelled` / `queued` are not normal completion; don't falsely report Stop.
        Some(other @ ("cancelled" | "queued")) => ModelFinishReason::Other(other.to_owned()),
        // completed / in_progress (won't occur at terminal state)
        _ => {
            if has_tool_calls {
                ModelFinishReason::ToolCalls
            } else {
                ModelFinishReason::Stop
            }
        }
    }
}

fn decode_usage(usage: Option<&dto::Usage>) -> message::Usage {
    let Some(usage) = usage else {
        return message::Usage::default();
    };
    let cached_input_tokens = usage
        .input_tokens_details
        .as_ref()
        .and_then(|details| details.cached_tokens);
    message::Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens,
        // Follow the contract: only treat input as the hit-rate denominator when it was reported.
        cache_reported_input_tokens: cached_input_tokens.map(|_| usage.input_tokens),
        reasoning_tokens: usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens),
    }
}

/// `status == "failed"` -> error. Shared by streaming and non-streaming, keeping the two paths'
/// contracts consistent.
fn response_error(response: &dto::Response) -> AdapterError {
    let detail = response.error.as_ref().map_or_else(
        || "response.failed".to_owned(),
        std::string::ToString::to_string,
    );
    AdapterError::Decode(format!("response failed: {detail}"))
}

pub fn decode_response(body: Value) -> Result<ModelResponse, AdapterError> {
    let response = serde_json::from_value::<dto::Response>(body)
        .map_err(|error| AdapterError::Decode(format!("failed to parse response: {error}")))?;
    if response.status.as_deref() == Some("failed") {
        return Err(response_error(&response));
    }
    Ok(decode_output(&response))
}

pub fn decode_stream_response(
    response: reqwest::Response,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>> {
    decode_stream_bytes(response.bytes_stream())
}

/// Streaming decode core: consume a byte stream and produce events.
///
/// Split from `decode_stream_response` for testability — swap the network layer
/// (`reqwest::Response`) for any byte stream and feed SSE samples in unit tests.
pub fn decode_stream_bytes<S>(
    byte_stream: S,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    Box::pin(async_stream::try_stream! {
        futures::pin_mut!(byte_stream);
        let mut buffer = String::new();
        // Real-time rendering relies on delta increments; but **the terminal state must not trust
        // `response.completed` alone**: some servers (in practice the DeepSeek compatibility
        // layer) **drop the reasoning item** in the streaming completed.output, keeping only the
        // message — relying on it alone would wipe the thinking content. So here, like
        // ChatCompletions, accumulate reasoning deltas and backfill them at Finished.
        let mut reasoning_content = String::new();
        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.map_err(AdapterError::Transport)?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            for section in sse::drain_sections(&mut buffer) {
                let Some(data) = sse::data_payload(&section) else {
                    continue;
                };
                if data.is_empty() || data == "[DONE]" {
                    // Responses doesn't send [DONE], but tolerate it in case a middle layer adds it.
                    continue;
                }
                let event = serde_json::from_str::<dto::StreamEvent>(&data).map_err(|error| {
                    AdapterError::Decode(format!("failed to deserialize SSE payload: {error}"))
                })?;

                match event {
                    dto::StreamEvent::OutputTextDelta { delta } => {
                        if !delta.is_empty() {
                            yield AdapterEvent::ContentDelta(delta);
                        }
                    }
                    dto::StreamEvent::ReasoningTextDelta { delta }
                    | dto::StreamEvent::ReasoningSummaryTextDelta { delta } => {
                        if !delta.is_empty() {
                            reasoning_content.push_str(&delta);
                            yield AdapterEvent::ReasoningDelta(delta);
                        }
                    }
                    dto::StreamEvent::Completed { response } => {
                        yield AdapterEvent::Finished(decode_output_with_reasoning(
                            &response,
                            &reasoning_content,
                        ));
                        return;
                    }
                    dto::StreamEvent::Incomplete { response } => {
                        // Truncation is also a legal terminal state; produce Finished as usual (finish_reason = Length).
                        yield AdapterEvent::Finished(decode_output_with_reasoning(
                            &response,
                            &reasoning_content,
                        ));
                        return;
                    }
                    dto::StreamEvent::Failed { response } => {
                        Err(response_error(&response))?;
                    }
                    dto::StreamEvent::Other => {}
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProtocol;

    fn config() -> ModelConfig {
        ModelConfig::builder()
            .protocol(ModelProtocol::Responses)
            .base_url("http://localhost/responses")
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

    #[test]
    fn system_stays_in_place_as_input_item() {
        // system is no longer hoisted to top-level instructions, but stays in place as an input item.
        let messages = vec![
            message::Message::System {
                content: "You are an assistant".into(),
            },
            message::Message::User {
                content: "hello".into(),
            },
        ];
        let body = body(&config(), &messages);
        assert!(
            body.get("instructions").is_none(),
            "top-level instructions are no longer used"
        );
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 2, "both system and user are in input");
        assert_eq!(input[0]["role"], serde_json::json!("system"));
        assert_eq!(
            input[0]["content"],
            serde_json::json!("You are an assistant")
        );
        assert_eq!(input[1]["role"], serde_json::json!("user"));
    }

    #[test]
    fn mid_list_system_stays_mid_list() {
        // A mid-list system must stay in place and never be moved to the front — otherwise prefix caching breaks.
        let messages = vec![
            message::Message::System {
                content: "initial".into(),
            },
            message::Message::User {
                content: "first question".into(),
            },
            message::Message::System {
                content: "in-round reminder".into(),
            },
            message::Message::User {
                content: "second question".into(),
            },
        ];
        let input = body(&config(), &messages)["input"].clone();
        let input = input.as_array().unwrap();
        let roles: Vec<_> = input.iter().map(|item| item["role"].clone()).collect();
        assert_eq!(
            roles,
            vec![
                serde_json::json!("system"),
                serde_json::json!("user"),
                serde_json::json!("system"),
                serde_json::json!("user"),
            ]
        );
        assert_eq!(input[2]["content"], serde_json::json!("in-round reminder"));
    }

    #[test]
    fn encoding_is_deterministic_and_prefix_stable() {
        // Contract 1: encoding the same input twice yields byte-identical results (no hash / no random).
        // Contract 2: after appending a message, the existing item prefix is unchanged (order preserved -> KV cache preserved).
        let base = vec![
            message::Message::System {
                content: "system".into(),
            },
            message::Message::User {
                content: "hello".into(),
            },
            message::Message::Assistant {
                content: Some("ok".into()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ];
        let first = body(&config(), &base)["input"].clone();
        let second = body(&config(), &base)["input"].clone();
        assert_eq!(first, second, "encoding the same input twice must be equal");

        let mut extended = base.clone();
        extended.push(message::Message::User {
            content: "appended".into(),
        });
        let extended_input = body(&config(), &extended)["input"].clone();
        let base_items = first.as_array().unwrap();
        let extended_items = extended_input.as_array().unwrap();
        assert_eq!(extended_items.len(), base_items.len() + 1);
        assert_eq!(
            &extended_items[..base_items.len()],
            base_items.as_slice(),
            "the prefix must be item-for-item unchanged after appending"
        );
    }

    #[test]
    fn assistant_with_text_and_tool_calls_expands_to_siblings() {
        let messages = vec![message::Message::Assistant {
            content: Some("let me check".into()),
            reasoning_content: Some("thinking".into()),
            tool_calls: vec![message::ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                arguments: "{\"command\":\"ls\"}".into(),
            }],
        }];
        let body = body(&config(), &messages);
        let input = body["input"].as_array().unwrap();
        assert_eq!(
            input.len(),
            2,
            "text and function_call should be sibling items"
        );
        assert_eq!(input[0]["type"], serde_json::json!("message"));
        assert_eq!(input[0]["role"], serde_json::json!("assistant"));
        // Historical assistant text uses plain string content (strictly conforming to EasyInputMessage).
        assert_eq!(input[0]["content"], serde_json::json!("let me check"));
        assert_eq!(input[1]["type"], serde_json::json!("function_call"));
        assert_eq!(input[1]["call_id"], serde_json::json!("call_1"));
        assert_eq!(input[1]["name"], serde_json::json!("bash"));
    }

    #[test]
    fn tool_result_becomes_function_call_output() {
        let messages = vec![message::Message::Tool {
            tool_call_id: "call_1".into(),
            content: Some("total 0".into()),
        }];
        let body = body(&config(), &messages);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], serde_json::json!("function_call_output"));
        assert_eq!(input[0]["call_id"], serde_json::json!("call_1"));
        assert_eq!(input[0]["output"], serde_json::json!("total 0"));
    }

    #[test]
    fn reasoning_effort_is_nested_and_max_output_tokens_key() {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::Responses)
            .base_url("http://localhost/responses")
            .model("deepseek-flash")
            .reasoning_effort("high")
            .max_output_tokens(2048)
            .temperature(0.3)
            .build();
        let body = body(&config, &[]);
        assert_eq!(body["reasoning"], serde_json::json!({ "effort": "high" }));
        assert_eq!(body["max_output_tokens"], serde_json::json!(2048));
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["temperature"], serde_json::json!(0.3));
    }

    #[test]
    fn tools_lack_function_wrapper() {
        let definition = tool::ToolDefinition {
            name: "bash".into(),
            description: "run a command".into(),
            parameters: serde_json::json!({ "type": "object" }),
        };
        let tools = vec![&definition];
        let body = encode_request(&config(), &request(&[], &tools))
            .unwrap()
            .body;
        let encoded = &body["tools"][0];
        assert_eq!(encoded["type"], serde_json::json!("function"));
        assert_eq!(encoded["name"], serde_json::json!("bash"));
        assert!(
            encoded.get("function").is_none(),
            "there should be no function wrapper layer"
        );
    }

    #[test]
    fn decodes_reasoning_message_and_function_call_together() {
        let body = serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                { "type": "reasoning", "content": [{ "type": "reasoning_text", "text": "think first" }] },
                { "type": "message", "role": "assistant",
                  "content": [{ "type": "output_text", "text": "answer" }] },
                { "type": "function_call", "call_id": "fc1", "name": "bash", "arguments": "{}" }
            ],
            "usage": {
                "input_tokens": 22,
                "input_tokens_details": { "cached_tokens": 5 },
                "output_tokens": 29,
                "output_tokens_details": { "reasoning_tokens": 27 }
            }
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
                assert_eq!(tool_calls[0].id, "fc1");
            }
            other => panic!("expected assistant, actual: {other:?}"),
        }
        assert_eq!(response.finish_reason, ModelFinishReason::ToolCalls);
        assert_eq!(response.usage.input_tokens, 22);
        assert_eq!(response.usage.output_tokens, 29);
        assert_eq!(response.usage.cached_input_tokens, Some(5));
        assert_eq!(response.usage.cache_reported_input_tokens, Some(22));
        assert_eq!(response.usage.reasoning_tokens, Some(27));
    }

    #[test]
    fn incomplete_maps_to_length() {
        let body = serde_json::json!({
            "id": "resp_1",
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [
                { "type": "message", "role": "assistant",
                  "content": [{ "type": "output_text", "text": "truncated" }] }
            ]
        });
        let response = decode_response(body).unwrap();
        assert_eq!(response.finish_reason, ModelFinishReason::Length);
    }

    #[test]
    fn reasoning_prefers_summary_over_content() {
        // OpenAI-family reasoning models return only `summary` by default (`content` is usually
        // empty); DeepSeek and others use `content`. Both must decode.
        let body = serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                { "type": "reasoning",
                  "summary": [{ "type": "summary_text", "text": "summary reasoning" }] },
                { "type": "message", "role": "assistant",
                  "content": [{ "type": "output_text", "text": "answer" }] }
            ]
        });
        let response = decode_response(body).unwrap();
        match response.message {
            message::Message::Assistant {
                reasoning_content, ..
            } => {
                assert_eq!(reasoning_content.as_deref(), Some("summary reasoning"));
            }
            other => panic!("expected assistant, actual: {other:?}"),
        }
    }

    #[test]
    fn reasoning_falls_back_to_content_when_summary_absent() {
        // DeepSeek style: only `content`, no `summary`.
        let body = serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                { "type": "reasoning",
                  "content": [{ "type": "reasoning_text", "text": "raw reasoning" }] },
                { "type": "message", "role": "assistant",
                  "content": [{ "type": "output_text", "text": "answer" }] }
            ]
        });
        let response = decode_response(body).unwrap();
        match response.message {
            message::Message::Assistant {
                reasoning_content, ..
            } => {
                assert_eq!(reasoning_content.as_deref(), Some("raw reasoning"));
            }
            other => panic!("expected assistant, actual: {other:?}"),
        }
    }

    #[test]
    fn failed_response_is_error_not_other() {
        // Non-streaming failed must become an error, consistent with the streaming Failed branch.
        let body = serde_json::json!({
            "id": "resp_1",
            "status": "failed",
            "error": { "code": "server_error", "message": "boom" },
            "output": []
        });
        let error = decode_response(body).expect_err("failed should become an error");
        assert!(
            error.to_string().contains("boom"),
            "the error should carry the detail: {error}"
        );
    }

    #[test]
    fn cancelled_status_is_not_reported_as_stop() {
        let body = serde_json::json!({
            "id": "resp_1",
            "status": "cancelled",
            "output": []
        });
        let response = decode_response(body).unwrap();
        assert_eq!(
            response.finish_reason,
            ModelFinishReason::Other("cancelled".to_owned())
        );
    }

    #[test]
    fn missing_cached_tokens_stays_none() {
        // An unreported cached_tokens must not be treated as 0: cache_hit_rate should return None.
        let body = serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "output": [],
            "usage": { "input_tokens": 10, "output_tokens": 3 }
        });
        let response = decode_response(body).unwrap();
        assert_eq!(response.usage.cached_input_tokens, None);
        assert_eq!(response.usage.cache_hit_rate(), None);
    }

    #[test]
    fn plain_string_content_is_accepted() {
        let body = serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                { "type": "message", "role": "assistant", "content": "plain string" }
            ]
        });
        let response = decode_response(body).unwrap();
        match response.message {
            message::Message::Assistant { content, .. } => {
                assert_eq!(content.as_deref(), Some("plain string"));
            }
            other => panic!("expected assistant, actual: {other:?}"),
        }
        assert_eq!(response.finish_reason, ModelFinishReason::Stop);
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
        // Key point: no `data: [DONE]`; it ends via response.completed.
        let chunks = vec![
            "event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":0}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
            "event: response.reasoning_text.delta\ndata: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"think\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}]}],\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n",
        ];
        let events = collect_stream(chunks).await;

        assert_eq!(events.len(), 3, "two deltas + one Finished");
        assert!(matches!(&events[0], AdapterEvent::ContentDelta(d) if d == "Hello"));
        assert!(matches!(&events[1], AdapterEvent::ReasoningDelta(d) if d == "think"));
        match &events[2] {
            AdapterEvent::Finished(response) => {
                assert_eq!(response.finish_reason, ModelFinishReason::Stop);
                match &response.message {
                    message::Message::Assistant { content, .. } => {
                        // The terminal state takes response.completed's full output
                        assert_eq!(content.as_deref(), Some("Hello world"));
                    }
                    other => panic!("expected assistant, actual: {other:?}"),
                }
            }
            other => panic!("the last event should be Finished, actual: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_accepts_reasoning_summary_delta() {
        // OpenAI-family reasoning streams use `response.reasoning_summary_text.delta` (field summary_index).
        let chunks = vec![
            "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"summary_index\":0,\"delta\":\"summary-think\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n",
        ];
        let events = collect_stream(chunks).await;
        assert!(
            matches!(&events[0], AdapterEvent::ReasoningDelta(d) if d == "summary-think"),
            "a summary delta should produce ReasoningDelta, actual: {:?}",
            events[0]
        );
        // Key regression: when completed.output is empty (server dropped the reasoning item),
        // Finished must backfill the accumulated reasoning, or the TUI thinking content is wiped.
        match &events[1] {
            AdapterEvent::Finished(response) => match &response.message {
                message::Message::Assistant {
                    reasoning_content, ..
                } => assert_eq!(
                    reasoning_content.as_deref(),
                    Some("summary-think"),
                    "the terminal state should backfill the accumulated reasoning"
                ),
                other => panic!("expected assistant, actual: {other:?}"),
            },
            other => panic!("expected Finished, actual: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_reasoning_from_output_wins_over_accumulated() {
        // When output **has** a reasoning item it wins (complete, authoritative), not overwritten by the accumulated value.
        let chunks = vec![
            "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"summary_index\":0,\"delta\":\"partial\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"full\"}]},{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}]}}\n\n",
        ];
        let events = collect_stream(chunks).await;
        match events.last().unwrap() {
            AdapterEvent::Finished(response) => match &response.message {
                message::Message::Assistant {
                    reasoning_content, ..
                } => assert_eq!(reasoning_content.as_deref(), Some("full")),
                other => panic!("expected assistant, actual: {other:?}"),
            },
            other => panic!("expected Finished, actual: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_handles_split_chunks() {
        // An event split across two chunks; framing must reassemble it correctly.
        let chunks = vec![
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"del",
            "ta\":\"part\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n",
        ];
        let events = collect_stream(chunks).await;
        assert!(matches!(&events[0], AdapterEvent::ContentDelta(d) if d == "part"));
        assert!(matches!(&events[1], AdapterEvent::Finished(_)));
    }

    #[tokio::test]
    async fn stream_failed_event_becomes_error() {
        let chunks = [
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"boom\"}}}\n\n",
        ];
        let stream = futures::stream::iter(vec![Ok::<_, reqwest::Error>(
            bytes::Bytes::from_static(chunks[0].as_bytes()),
        )]);
        let mut decoded = decode_stream_bytes(stream);
        let event = decoded.next().await.expect("there should be one event");
        let error = event.expect_err("failed should become an error");
        assert!(
            error.to_string().contains("boom"),
            "the error should carry the detail: {error}"
        );
    }

    /// Real-network end-to-end test (ignored by default).
    ///
    /// Requires environment variables:
    /// - `DEEPSEEK_API_KEY` (required; if missing, skip and return)
    /// - `SHIRLEY_RESPONSES_BASE_URL` (optional, default `https://api.deepseek.com/responses`)
    /// - `SHIRLEY_RESPONSES_MODEL` (optional, default `deepseek-flash`)
    ///
    /// How to run:
    /// ```sh
    /// cargo test -p shirley-agent-sdk --lib responses::tests::live -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "real network call, requires DEEPSEEK_API_KEY"]
    async fn live_responses_round_trip() {
        let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
            eprintln!("skipping: DEEPSEEK_API_KEY not set");
            return;
        };
        let base_url = std::env::var("SHIRLEY_RESPONSES_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/responses".to_owned());
        let model = std::env::var("SHIRLEY_RESPONSES_MODEL")
            .unwrap_or_else(|_| "deepseek-flash".to_owned());

        let config = ModelConfig::builder()
            .protocol(ModelProtocol::Responses)
            .base_url(base_url)
            .model(model)
            .api_key(api_key)
            .stream(true)
            .build();

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
        let mut stream = crate::adapter::invoke(&client, &config, request);

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut finished = None;
        while let Some(event) = stream.next().await {
            match event.expect("streaming events should not error") {
                AdapterEvent::Retrying { .. } | AdapterEvent::ImagesDropped { .. } => {}
                AdapterEvent::ContentDelta(delta) => content.push_str(&delta),
                AdapterEvent::ReasoningDelta(delta) => reasoning.push_str(&delta),
                AdapterEvent::Finished(response) => {
                    finished = Some(response);
                    break;
                }
            }
        }

        let finished = finished.expect("the stream should produce Finished");
        eprintln!("content   = {content:?}");
        eprintln!("reasoning = {} chars", reasoning.len());
        eprintln!("finish    = {:?}", finished.finish_reason);
        eprintln!("usage     = {:?}", finished.usage);

        assert!(
            !content.trim().is_empty(),
            "should get non-empty text output, actual: {content:?}"
        );
        assert!(
            finished.usage.input_tokens > 0,
            "usage.input_tokens should be > 0"
        );
    }

    /// Real-network test (ignored by default): whether **a mid-list system** is accepted by `DeepSeek`.
    ///
    /// This is the sole hard prerequisite for the "drop top-level `instructions`, keep system in
    /// place as an input item" approach. If `DeepSeek` rejects `role:"system"` inside input, this
    /// test errors.
    #[tokio::test]
    #[ignore = "real network call, requires DEEPSEEK_API_KEY"]
    async fn live_responses_mid_list_system_is_accepted() {
        let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
            eprintln!("skipping: DEEPSEEK_API_KEY not set");
            return;
        };
        let base_url = std::env::var("SHIRLEY_RESPONSES_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/responses".to_owned());
        let model = std::env::var("SHIRLEY_RESPONSES_MODEL")
            .unwrap_or_else(|_| "deepseek-flash".to_owned());

        let config = ModelConfig::builder()
            .protocol(ModelProtocol::Responses)
            .base_url(base_url)
            .model(model)
            .api_key(api_key)
            .stream(false)
            .build();

        // Deliberately place system **mid-list** to verify an input item's `role:"system"` is accepted.
        let messages = vec![
            message::Message::User {
                content: "Remember a word: apple.".into(),
            },
            message::Message::System {
                content: "You may only answer with fruit names now.".into(),
            },
            message::Message::User {
                content: "What was the word I asked you to remember?".into(),
            },
        ];
        let tools: Vec<&tool::ToolDefinition> = vec![];
        let request = ModelRequest {
            messages: &messages,
            tools: &tools,
            session_id: None,
        };

        let client = reqwest::Client::new();
        let response = crate::adapter::invoke(&client, &config, request)
            .next()
            .await
            .expect("the stream should produce events")
            .expect("a mid-list system should be accepted, not error");
        match response {
            AdapterEvent::Finished(response) => {
                eprintln!("finish = {:?}", response.finish_reason);
            }
            other => panic!("should produce Finished, actual: {other:?}"),
        }
    }

    /// Real-network **tool round-trip** test (ignored by default): the model emits a
    /// `function_call` on the first turn, we send back a `function_call_output`, and the second turn
    /// gets the final text.
    ///
    /// This specifically verifies whether the request encoding's `function_call` /
    /// `function_call_output` paired by `call_id` is accepted by the server.
    #[tokio::test]
    #[ignore = "real network call, requires DEEPSEEK_API_KEY"]
    async fn live_responses_tool_round_trip() {
        let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
            eprintln!("skipping: DEEPSEEK_API_KEY not set");
            return;
        };
        let base_url = std::env::var("SHIRLEY_RESPONSES_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/responses".to_owned());
        let model = std::env::var("SHIRLEY_RESPONSES_MODEL")
            .unwrap_or_else(|_| "deepseek-flash".to_owned());

        let config = ModelConfig::builder()
            .protocol(ModelProtocol::Responses)
            .base_url(base_url)
            .model(model)
            .api_key(api_key)
            .stream(false)
            .build();

        let tool_definition = tool::ToolDefinition {
            name: "get_weather".into(),
            description: "look up city weather".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
                "additionalProperties": false
            }),
        };
        let tools = vec![&tool_definition];
        let client = reqwest::Client::new();

        // First turn: expect the model to emit a function_call.
        let first_messages = vec![
            message::Message::System {
                content: "You are an assistant.".into(),
            },
            message::Message::User {
                content: "What's the weather in Beijing now? Look it up with the tool.".into(),
            },
        ];
        let first = run_once(&client, &config, &first_messages, &tools).await;
        let (call_id, call_name) = match &first.message {
            message::Message::Assistant { tool_calls, .. } => {
                let call = tool_calls.first().expect("should emit one tool call");
                eprintln!("tool_call = {} / {}", call.id, call.name);
                (call.id.clone(), call.name.clone())
            }
            other => {
                panic!("the first turn should be an assistant with tool calls, actual: {other:?}")
            }
        };
        assert_eq!(first.finish_reason, ModelFinishReason::ToolCalls);
        assert_eq!(call_name, "get_weather");

        // Second turn: send back the function_call_output.
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

    /// Run one non-streaming request and return the single Finished.
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
        let mut stream = crate::adapter::invoke(client, config, request);
        while let Some(event) = stream.next().await {
            if let AdapterEvent::Finished(response) = event.expect("the request should not error") {
                return response;
            }
        }
        panic!("the stream should produce Finished");
    }

    /// Real-network **streaming tool call** test (ignored by default).
    ///
    /// Locks in a key design bet: the terminal `response.completed` carries the full `output`
    /// (including `function_call`), so we need not aggregate `function_call_arguments.delta`.
    #[tokio::test]
    #[ignore = "real network call, requires DEEPSEEK_API_KEY"]
    async fn live_responses_streaming_tool_call() {
        let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
            eprintln!("skipping: DEEPSEEK_API_KEY not set");
            return;
        };
        let base_url = std::env::var("SHIRLEY_RESPONSES_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/responses".to_owned());
        let model = std::env::var("SHIRLEY_RESPONSES_MODEL")
            .unwrap_or_else(|_| "deepseek-flash".to_owned());

        let config = ModelConfig::builder()
            .protocol(ModelProtocol::Responses)
            .base_url(base_url)
            .model(model)
            .api_key(api_key)
            .stream(true)
            .build();

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

        let client = reqwest::Client::new();
        let mut stream = crate::adapter::invoke(&client, &config, request);
        let mut reasoning = String::new();
        let mut finished = None;
        while let Some(event) = stream.next().await {
            match event.expect("streaming events should not error") {
                AdapterEvent::Retrying { .. }
                | AdapterEvent::ImagesDropped { .. }
                | AdapterEvent::ContentDelta(_) => {}
                AdapterEvent::ReasoningDelta(delta) => reasoning.push_str(&delta),
                AdapterEvent::Finished(response) => {
                    finished = Some(response);
                    break;
                }
            }
        }

        let finished = finished.expect("the stream should produce Finished");
        eprintln!("reasoning = {} chars", reasoning.len());
        eprintln!("finish    = {:?}", finished.finish_reason);
        match &finished.message {
            message::Message::Assistant { tool_calls, .. } => {
                let call = tool_calls
                    .first()
                    .expect("the terminal state should contain a function_call");
                eprintln!(
                    "tool_call = {} / {} / {}",
                    call.id, call.name, call.arguments
                );
                assert_eq!(call.name, "get_weather");
                assert!(!call.id.is_empty(), "call_id should not be empty");
                assert!(
                    call.arguments.contains("city"),
                    "arguments should be complete JSON"
                );
            }
            other => panic!("expected an assistant with tool calls, actual: {other:?}"),
        }
        assert_eq!(finished.finish_reason, ModelFinishReason::ToolCalls);
        assert!(
            !reasoning.is_empty(),
            "streaming should produce reasoning deltas"
        );
    }
}
