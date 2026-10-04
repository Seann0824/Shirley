//! Anthropic Messages 协议适配（`POST /v1/messages`）。
//!
//! 依据（真实抓包，见 `docs/anthropic-messages-api.md`）：
//! - 端点 `https://api.deepseek.com/anthropic/v1/messages`
//! - 请求头 **`x-api-key`** + `anthropic-version`（不是 Bearer）
//!
//! 与另两个协议的三条要命差异：
//! 1. **工具结果放在 `user` 消息里**（Anthropic 没有 tool role），是 `tool_result` block；
//! 2. thinking 通过 `reasoning_content` 承载（`signature` 无需回传，实测不校验）；
//! 3. **`input_tokens` 不含缓存读取**（与 OpenAI 相反），映射时要加回 `cache_read`。

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

/// Anthropic 协议版本头，随请求固定发送。
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// 未显式指定 `max_output_tokens` 时的兜底值。
///
/// Anthropic 规范要求 `max_tokens` 必填（DeepSeek 兼容端点缺省也接受，但真 Anthropic
/// 不），给个默认值以保跨供应商可移植。
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// 消息 → `(system, messages)`。
///
/// `System` / `ContextSummary` 摘到顶层 `system`；其余转成 content block 消息。
fn encode_messages(messages: &[message::Message]) -> (Option<String>, Vec<Value>) {
    let mut system: Vec<String> = Vec::new();
    let mut encoded: Vec<Value> = Vec::new();

    for message in messages {
        match message {
            message::Message::System { content } => system.push(content.clone()),
            message::Message::ContextSummary { content } => system.push(content.clone()),

            message::Message::User { content } => encoded.push(serde_json::json!({
                "role": "user",
                "content": [{ "type": "text", "text": content }],
            })),

            // 块顺序：thinking → text → tool_use。thinking 必须在前。
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
                    // 内部 arguments 是 JSON 字符串；Anthropic 要 JSON 对象。
                    let input: Value = serde_json::from_str(&call.arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": &call.id,
                        "name": &call.name,
                        "input": input,
                    }));
                }
                // 空 assistant（既无文本也无工具调用）不产出，避免空 content 数组。
                if !blocks.is_empty() {
                    encoded.push(serde_json::json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                }
            }

            // 工具结果在 Anthropic 里是 **user 消息**里的 `tool_result` block。
            message::Message::Tool {
                tool_call_id,
                content,
            } => encoded.push(serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": content.clone().unwrap_or_default(),
                }],
            })),
        }
    }

    let system = if system.is_empty() {
        None
    } else {
        Some(system.join("\n\n"))
    };
    (system, encoded)
}

/// 工具定义 → Anthropic 格式（`input_schema` 就是 JSON Schema）。
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
        // Anthropic 用 `x-api-key`，不是 `Authorization: Bearer`。
        let mut api_key_header = HeaderValue::from_str(api_key).map_err(|error| {
            AdapterError::Encode(format!("API key is not a valid header value: {error}"))
        })?;
        api_key_header.set_sensitive(true);
        headers.insert("x-api-key", api_key_header);
    }

    let (system, messages) = encode_messages(input.messages);
    let mut body = serde_json::json!({
        "model": &config.model,
        // Anthropic 的 `max_tokens` 必填；无配置时给兜底值。
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
    // thinking 在 Anthropic 里是对象 `{type:"enabled"}`；`budget_tokens` 走 extra_body。
    if config.thinking {
        body["thinking"] = serde_json::json!({ "type": "enabled" });
    }
    // 注意：`reasoning_effort` 在 Anthropic 协议下没有对应字段，**刻意忽略**。
    if let Some(tool_choice) = &config.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }

    // 逃生口最后应用（浅合并，null 删键），语义与另两个协议一致。
    if let Some(extra) = &config.extra_body {
        apply_extra_body(&mut body, extra);
    }

    Ok(PreparedRequest {
        url: config.base_url.clone(),
        headers,
        body,
    })
}

/// 浅合并 `extra` 进请求体：应用键覆盖标准键，`null` 删除键，非对象忽略。
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

/// `content[]` → 内部 assistant 消息。
///
/// 多块合成一条：thinking → `reasoning_content`、
/// text → `content`（拼接）、tool_use → `ToolCall`。
fn decode_content(content: &[dto::ContentBlock]) -> message::Message {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut tool_calls = Vec::new();

    for block in content {
        match block {
            dto::ContentBlock::Text { text: chunk } => text.push_str(chunk),
            // signature 不回传（实测服务端不校验；见 docs/anthropic-messages-api.md）。
            dto::ContentBlock::Thinking { thinking: chunk, .. } => thinking.push_str(chunk),
            dto::ContentBlock::ToolUse { id, name, input } => {
                if id.is_empty() {
                    continue;
                }
                tool_calls.push(message::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    // input 是对象 → 内部契约为字符串。
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
/// 优先看 content 里有没有 `tool_use`（与 Responses 一致的"不读字符串"原则），
/// `stop_reason` 作为辅助。
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
        // end_turn / stop_sequence / None 都算正常结束。
        _ => ModelFinishReason::Stop,
    }
}

/// usage 映射。**这里必须把 `cache_read` 加回 `input_tokens`**——
/// Anthropic 的 `input_tokens` 不含缓存读取（与 OpenAI 相反）。
fn decode_usage(usage: Option<&dto::Usage>) -> message::Usage {
    let Some(usage) = usage else {
        return message::Usage::default();
    };
    let total_input = usage.input_tokens + usage.cache_read_input_tokens.unwrap_or(0);
    message::Usage {
        input_tokens: total_input,
        output_tokens: usage.output_tokens,
        // 命中 = 缓存读取；服务端上报了字段才 Some（否则 None，不当 0）。
        cached_input_tokens: usage.cache_read_input_tokens,
        cache_reported_input_tokens: usage.cache_read_input_tokens.map(|_| total_input),
        // Anthropic 不单独上报推理 token（thinking 计入 output_tokens）。
        reasoning_tokens: None,
    }
}

pub fn decode_response(body: Value) -> Result<ModelResponse, AdapterError> {
    let response = serde_json::from_value::<dto::MessageResponse>(body).map_err(|error| {
        AdapterError::Decode(format!("failed to parse response: {error}"))
    })?;
    Ok(ModelResponse {
        message: decode_content(&response.content),
        finish_reason: finish_reason(&response),
        usage: decode_usage(response.usage.as_ref()),
    })
}

/// 流式解码核心。与网络层拆开以便测试。
pub fn decode_stream_bytes<S>(
    byte_stream: S,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    Box::pin(async_stream::try_stream! {
        futures::pin_mut!(byte_stream);
        let mut buffer = String::new();

        // 流内聚合状态。
        let mut text = String::new();
        let mut thinking = String::new();
        // 按 content block 的 `index` 聚合工具参数（input_json_delta 分片拼接）。
        let mut tools: Vec<(usize, String, String, String)> = Vec::new(); // (index, id, name, args)
        // usage 分两处给：message_start 给 input 侧，message_delta 给 output 侧。
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
                        // signature 不回传，忽略（实测服务端不校验）。
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

/// 把流内聚合状态拼成最终响应。
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

    // 合并两处 usage：input 侧取 message_start，output 侧取 message_delta。
    let merged = merge_stream_usage(input_usage, output_usage);

    // finish_reason 复用同一套判定：有 tool_use 就是 ToolCalls。
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

/// 合并流式分两处上报的 usage。
fn merge_stream_usage(
    input_usage: Option<&dto::Usage>,
    output_usage: Option<&dto::Usage>,
) -> message::Usage {
    let Some(input) = input_usage else {
        // 没有 input 侧就退回 output 侧（至少拿到 output_tokens）。
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
        ModelRequest { messages, tools }
    }

    fn body(config: &ModelConfig, messages: &[message::Message]) -> Value {
        let tools: Vec<&tool::ToolDefinition> = vec![];
        encode_request(config, &request(messages, &tools))
            .expect("编码应成功")
            .body
    }

    fn sample_tool() -> tool::ToolDefinition {
        tool::ToolDefinition {
            name: "bash".into(),
            description: "执行命令".into(),
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
            encode_request(&config, &request(&[], &tools)).expect("编码应成功");
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
                content: "你是助手".into(),
            },
            message::Message::ContextSummary {
                content: "摘要".into(),
            },
            message::Message::User {
                content: "你好".into(),
            },
        ];
        let body = body(&config(), &messages);
        assert_eq!(body["system"], serde_json::json!("你是助手\n\n摘要"));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "system/summary 不应进入 messages");
        assert_eq!(messages[0]["role"], serde_json::json!("user"));
    }

    #[test]
    fn user_message_uses_text_block() {
        let messages = vec![message::Message::User {
            content: "你好".into(),
        }];
        let body = body(&config(), &messages);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"][0]["type"], serde_json::json!("text"));
        assert_eq!(messages[0]["content"][0]["text"], serde_json::json!("你好"));
    }

    #[test]
    fn assistant_block_order_is_thinking_text_tool_use() {
        let messages = vec![message::Message::Assistant {
            content: Some("我来查".into()),
            reasoning_content: Some("先想".into()),
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
        assert_eq!(blocks[0]["thinking"], serde_json::json!("先想"));
        // signature 不回传（实测服务端不校验），编码时不应出现。
        assert!(blocks[0].get("signature").is_none());
        assert_eq!(blocks[1]["type"], serde_json::json!("text"));
        assert_eq!(blocks[2]["type"], serde_json::json!("tool_use"));
        // input 必须是 JSON 对象，不是字符串。
        assert_eq!(blocks[2]["input"], serde_json::json!({"command": "ls"}));
    }

    #[test]
    fn tool_message_becomes_user_tool_result_block() {
        let messages = vec![message::Message::Tool {
            tool_call_id: "call_1".into(),
            content: Some("晴，25℃".into()),
        }];
        let body = body(&config(), &messages);
        let messages = body["messages"].as_array().unwrap();
        // 工具结果在 Anthropic 里是 user 消息，不是 tool role。
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
            serde_json::json!("晴，25℃")
        );
    }

    #[test]
    fn tool_schema_uses_input_schema_key() {
        let tool = sample_tool();
        let tools = vec![&tool];
        let prepared = encode_request(&config(), &request(&[], &tools)).unwrap();
        let encoded = &prepared.body["tools"][0];
        assert_eq!(encoded["name"], serde_json::json!("bash"));
        assert_eq!(encoded["description"], serde_json::json!("执行命令"));
        // 键名是 input_schema，值就是 JSON Schema。
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
            "Anthropic 无此字段，应忽略"
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
        let body = body(&config, &[message::Message::System { content: "s".into() }]);
        assert_eq!(body["thinking"]["budget_tokens"], serde_json::json!(2048));
        assert!(body.get("system").is_none(), "null 应删除键");
    }

    #[test]
    fn decode_content_blocks_into_single_assistant() {
        let body = serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "先想", "signature": "sig-1" },
                { "type": "text", "text": "答案" },
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
                assert_eq!(content.as_deref(), Some("答案"));
                assert_eq!(reasoning_content.as_deref(), Some("先想"));
                assert_eq!(tool_calls.len(), 1);
                // input 对象 → arguments 字符串。
                assert_eq!(tool_calls[0].arguments, "{\"command\":\"ls\"}");
            }
            other => panic!("expected assistant, got {other:?}"),
        }
        assert_eq!(response.finish_reason, ModelFinishReason::ToolCalls);
    }

    #[test]
    fn decode_usage_adds_cache_read_back() {
        // 真实抓包：input=134, cache_read=2301 → 总输入 2435。
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
        assert_eq!(response.usage.input_tokens, 2435, "input 必须加回 cache_read");
        assert_eq!(response.usage.cached_input_tokens, Some(2301));
        assert_eq!(response.usage.cache_reported_input_tokens, Some(2435));
        let rate = response.usage.cache_hit_rate().unwrap();
        assert!((rate - 2301.0 / 2435.0).abs() < 1e-9, "命中率应在 (0,1) 内");
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
        assert_eq!(response.usage.cached_input_tokens, None, "未上报应为 None");
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
            assert_eq!(response.finish_reason, expected, "stop_reason={stop_reason}");
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

    /// 把一串 SSE 文本喂给流式解码核心，收集事件。
    async fn collect_stream(chunks: Vec<&'static str>) -> Vec<AdapterEvent> {
        use futures::StreamExt;
        let stream = futures::stream::iter(
            chunks
                .into_iter()
                .map(|c| Ok::<_, reqwest::Error>(bytes::Bytes::from_static(c.as_bytes())))
                .collect::<Vec<_>>(),
        );
        let mut events = Vec::new();
        let mut decoded = decode_stream_bytes(stream);
        while let Some(event) = decoded.next().await {
            events.push(event.expect("流式解码不应报错"));
        }
        events
    }

    #[tokio::test]
    async fn stream_yields_deltas_and_finishes_without_done_marker() {
        // 关键点：没有 `data: [DONE]`，靠 message_stop 收尾。
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
                    other => panic!("应为 assistant，实际: {other:?}"),
                }
                // usage 分两处给：input 侧 + output 侧，必须合并。
                assert_eq!(response.usage.input_tokens, 14, "10 + cache_read 4");
                assert_eq!(response.usage.cached_input_tokens, Some(4));
                assert_eq!(response.usage.output_tokens, 2);
            }
            other => panic!("最后一个事件应为 Finished，实际: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_aggregates_input_json_delta_fragments() {
        // Anthropic 的 tool_use.input 只能靠 input_json_delta 分片拼出来。
        let chunks = vec![
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"bash\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\":\\\"ls\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        let events = collect_stream(chunks).await;
        assert_eq!(events.len(), 1, "仅一个 Finished（无 delta 文本）");
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
                    other => panic!("应为含工具调用的 assistant，实际: {other:?}"),
                }
            }
            other => panic!("应为 Finished，实际: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_handles_split_chunks() {
        // 一个事件被拆到两个 chunk 里，分帧要能正确拼接。
        let chunks = vec![
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_del",
            "ta\",\"text\":\"part\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        let events = collect_stream(chunks).await;
        assert!(matches!(&events[0], AdapterEvent::ContentDelta(d) if d == "part"));
        assert!(matches!(&events[1], AdapterEvent::Finished(_)));
    }

    /// 真实网络端到端测试（默认 ignored）。
    ///
    /// 需要环境变量：
    /// - `DEEPSEEK_API_KEY`（必填，缺失即跳过并返回）
    /// - `SHIRLEY_ANTHROPIC_BASE_URL`（可选，默认 `https://api.deepseek.com/anthropic/v1/messages`）
    /// - `SHIRLEY_ANTHROPIC_MODEL`（可选，默认 `deepseek-flash`）
    ///
    /// 跑法：
    /// ```sh
    /// cargo test -p shirley-agent-sdk --lib anthropic_messages::tests::live -- --ignored --nocapture
    /// ```
    fn live_config(stream: bool) -> Option<ModelConfig> {
        let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
            eprintln!("跳过：未设置 DEEPSEEK_API_KEY");
            return None;
        };
        let base_url = std::env::var("SHIRLEY_ANTHROPIC_BASE_URL").unwrap_or_else(|_| {
            "https://api.deepseek.com/anthropic/v1/messages".to_owned()
        });
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
        use futures::StreamExt;
        let request = ModelRequest { messages, tools };
        let mut stream = crate::adapter::invoke(client, config, request).await;
        while let Some(event) = stream.next().await {
            if let AdapterEvent::Finished(response) = event.expect("请求不应报错") {
                return response;
            }
        }
        panic!("流应产出 Finished");
    }

    #[tokio::test]
    #[ignore = "真实网络调用，需 DEEPSEEK_API_KEY"]
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
        };
        let client = reqwest::Client::new();
        use futures::StreamExt;
        let mut stream = crate::adapter::invoke(&client, &config, request).await;

        let mut content = String::new();
        let mut finished = None;
        while let Some(event) = stream.next().await {
            match event.expect("流式事件不应报错") {
                AdapterEvent::ContentDelta(delta) => content.push_str(&delta),
                AdapterEvent::ReasoningDelta(_) => {}
                AdapterEvent::Finished(response) => {
                    finished = Some(response);
                    break;
                }
            }
        }
        let finished = finished.expect("流应产出 Finished");
        eprintln!("content = {content:?}");
        eprintln!("finish  = {:?}", finished.finish_reason);
        eprintln!("usage   = {:?}", finished.usage);
        assert!(!content.trim().is_empty(), "应拿到非空文本输出");
        assert!(finished.usage.input_tokens > 0, "usage.input_tokens 应 > 0");
    }

    /// 真实网络的**工具往返**测试（默认 ignored），验证最关键的链路：
    /// 第一轮模型发起 `tool_use`（并可能产生 thinking），
    /// 我们回传 assistant（含 thinking 块）与 `tool_result`，
    /// 第二轮拿到最终文本。
    ///
    /// 这一步锁死：`tool_result` 必须放在 user 消息里（Anthropic 无 tool role）。
    #[tokio::test]
    #[ignore = "真实网络调用，需 DEEPSEEK_API_KEY"]
    async fn live_anthropic_tool_round_trip() {
        let Some(config) = live_config(false) else {
            return;
        };
        let tool_definition = tool::ToolDefinition {
            name: "get_weather".into(),
            description: "查询城市天气".into(),
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
                content: "你是助手。".into(),
            },
            message::Message::User {
                content: "北京现在天气如何？用工具查。".into(),
            },
        ];
        let first = run_once(&client, &config, &first_messages, &tools).await;
        let call_id = match &first.message {
            message::Message::Assistant { tool_calls, .. } => {
                let call = tool_calls.first().expect("应发起一次工具调用");
                eprintln!("tool_call = {} / {}", call.id, call.name);
                assert_eq!(call.name, "get_weather");
                assert!(call.arguments.contains("city"), "arguments 应含 city");
                call.id.clone()
            }
            other => panic!("第一轮应为含工具调用的 assistant，实际: {other:?}"),
        };
        assert_eq!(first.finish_reason, ModelFinishReason::ToolCalls);

        // 第二轮：回传 first.message（含 thinking）+ tool_result。
        let second_messages = vec![
            message::Message::System {
                content: "你是助手。".into(),
            },
            message::Message::User {
                content: "北京现在天气如何？用工具查。".into(),
            },
            first.message.clone(),
            message::Message::Tool {
                tool_call_id: call_id,
                content: Some("晴，25℃".into()),
            },
        ];
        let second = run_once(&client, &config, &second_messages, &tools).await;
        match &second.message {
            message::Message::Assistant { content, .. } => {
                let text = content.clone().unwrap_or_default();
                eprintln!("final = {text:?}");
                assert!(!text.trim().is_empty(), "第二轮应给出最终文本");
            }
            other => panic!("第二轮应为文本 assistant，实际: {other:?}"),
        }
        assert_eq!(second.finish_reason, ModelFinishReason::Stop);
    }

    /// 真实网络的**流式工具调用**测试（默认 ignored）。
    ///
    /// 与 Responses 相反：终态**不带**完整 input，必须靠 `input_json_delta`
    /// 分片聚合出 arguments。这一步锁死该聚合逻辑。
    #[tokio::test]
    #[ignore = "真实网络调用，需 DEEPSEEK_API_KEY"]
    async fn live_anthropic_streaming_tool_call() {
        let Some(config) = live_config(true) else {
            return;
        };
        let tool_definition = tool::ToolDefinition {
            name: "get_weather".into(),
            description: "查询城市天气".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        };
        let tools = vec![&tool_definition];
        let messages = vec![
            message::Message::System {
                content: "你是助手。".into(),
            },
            message::Message::User {
                content: "北京天气？用工具查。".into(),
            },
        ];
        let request = ModelRequest {
            messages: &messages,
            tools: &tools,
        };
        use futures::StreamExt;
        let client = reqwest::Client::new();
        let mut stream = crate::adapter::invoke(&client, &config, request).await;
        let mut reasoning = String::new();
        let mut finished = None;
        while let Some(event) = stream.next().await {
            match event.expect("流式事件不应报错") {
                AdapterEvent::ReasoningDelta(delta) => reasoning.push_str(&delta),
                AdapterEvent::ContentDelta(_) => {}
                AdapterEvent::Finished(response) => {
                    finished = Some(response);
                    break;
                }
            }
        }
        let finished = finished.expect("流应产出 Finished");
        eprintln!("reasoning = {} chars", reasoning.len());
        match &finished.message {
            message::Message::Assistant { tool_calls, .. } => {
                let call = tool_calls.first().expect("终态应含 tool_use");
                eprintln!("tool_call = {} / {} / {}", call.id, call.name, call.arguments);
                assert_eq!(call.name, "get_weather");
                assert!(!call.id.is_empty(), "tool_use id 不应为空");
                assert!(call.arguments.contains("city"), "arguments 应为聚合后的完整 JSON");
            }
            other => panic!("应为含工具调用的 assistant，实际: {other:?}"),
        }
        assert_eq!(finished.finish_reason, ModelFinishReason::ToolCalls);
    }
}
