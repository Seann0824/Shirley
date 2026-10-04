//! Responses 协议适配（`POST /responses`）。
//!
//! 依据：
//! - <https://api-docs.deepseek.com/guides/responses_api/>
//! - <https://api-docs.deepseek.com/zh-cn/api/create-response>
//!
//! 与 ChatCompletions 的差异（详见 `docs/responses-api.md`）：
//! - system 走顶层 `instructions`，不放进 `input`；
//! - `input` 是 **item 列表**：一条 assistant 的文本与它发出的 `function_call`
//!   是**并列的兄弟 item**（一对多展开）；工具结果走 `function_call_output`；
//! - 工具定义少一层 `function` 包装；
//! - 流式**没有 `data: [DONE]`**，以 `response.completed` / `incomplete` /
//!   `failed` 收尾。

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

/// 消息 → `input` item 列表。
///
/// 返回 `(instructions, input)`：`System` / `ContextSummary` 被摘出来拼进
/// `instructions`（顶层字段），其余消息展开为 item。
///
/// **一对多**：一条含文本 + tool_calls 的 assistant 会产出「文本 item +
/// 若干 function_call item」。
fn encode_input(messages: &[message::Message]) -> (Option<String>, Vec<Value>) {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    for message in messages {
        match message {
            // system 级内容不进 input，收敛到顶层 instructions。
            message::Message::System { content } => instructions.push(content.clone()),
            message::Message::ContextSummary { content } => instructions.push(content.clone()),

            message::Message::User { content } => input.push(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": content }],
            })),

            // 历史思考（reasoning_content）刻意不回传：服务端接受 reasoning item，
            // 但把旧思考再喂回去没有收益；需要时走 extra_body。
            message::Message::Assistant {
                content,
                reasoning_content: _,
                tool_calls,
            } => {
                // 文本与工具调用是兄弟 item，分别 push。
                if let Some(content) = content
                    && !content.is_empty()
                {
                    input.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": content }],
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
            } => input.push(serde_json::json!({
                "type": "function_call_output",
                "call_id": tool_call_id,
                // output 允许字符串或内容块数组；纯文本用字符串最直接。
                "output": content.clone().unwrap_or_default(),
            })),
        }
    }

    let instructions = if instructions.is_empty() {
        None
    } else {
        Some(instructions.join("\n\n"))
    };
    (instructions, input)
}

/// 工具定义 → Responses 格式（少一层 `function` 包装）。
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
        let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(
            |error| AdapterError::Encode(format!("API key is not a valid header value: {error}")),
        )?;
        authorization.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
    }

    let (instructions, items) = encode_input(input.messages);
    let mut body = serde_json::json!({
        "model": &config.model,
        "input": items,
        "tools": encode_tools(input.tools),
        "stream": &config.stream,
    });

    // instructions 与 input 至少有一个；我们总有 input，所以为空时干脆不发。
    if let Some(instructions) = instructions {
        body["instructions"] = serde_json::json!(instructions);
    }

    if let Some(temperature) = config.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    // Responses 的键名就是 `max_output_tokens`（ChatCompletions 才是 `max_tokens`）。
    if let Some(max_output_tokens) = config.max_output_tokens {
        body["max_output_tokens"] = serde_json::json!(max_output_tokens);
    }
    // reasoning_effort 在这里要包成嵌套对象 `{"reasoning": {"effort": ...}}`，
    // 与 ChatCompletions 的顶层字符串形状不同。
    if let Some(reasoning_effort) = &config.reasoning_effort {
        body["reasoning"] = serde_json::json!({ "effort": reasoning_effort });
    }
    if let Some(tool_choice) = &config.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }

    // 逃生口最后应用，语义与 ChatCompletions 一致（浅合并，null 删键）。
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

/// 把 `output[]` 摊平成一条内部 assistant 消息。
///
/// 多对一：reasoning / message / function_call 三类 item 合成
/// `Message::Assistant { content, reasoning_content, tool_calls }`。
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
                if let Some(part) = &reasoning.content {
                    reasoning_content.push_str(&part.text());
                }
            }
            dto::OutputItem::FunctionCall(call) => {
                let id = call.call_id.clone().unwrap_or_default();
                if id.is_empty() {
                    // 缺 call_id 无法配对工具结果，交由上层报错更安全，
                    // 但这里保持宽容：空 id 在工具执行阶段会被拒绝。
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

/// finish_reason 判定：Responses **没有** `finish_reason` 字段，
/// 工具调用要看 `output` 里有没有 `function_call`。
fn finish_reason(response: &dto::Response, has_tool_calls: bool) -> ModelFinishReason {
    match response.status.as_deref() {
        Some("incomplete") => match response
            .incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref())
        {
            // 达到 max_output_tokens（也可能是 content_filter，都归 Length 之外的 Other）
            Some("max_output_tokens") => ModelFinishReason::Length,
            Some(other) => ModelFinishReason::Other(other.to_owned()),
            None => ModelFinishReason::Other("incomplete".to_owned()),
        },
        Some("failed") => ModelFinishReason::Other("failed".to_owned()),
        // completed / in_progress（终态时不会出现）
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
        // 沿用契约：只有上报了才把 input 当作命中率分母。
        cache_reported_input_tokens: cached_input_tokens.map(|_| usage.input_tokens),
        reasoning_tokens: usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens),
    }
}

pub fn decode_response(body: Value) -> Result<ModelResponse, AdapterError> {
    let response = serde_json::from_value::<dto::Response>(body)
        .map_err(|error| AdapterError::Decode(format!("failed to parse response: {error}")))?;
    Ok(decode_output(&response))
}

pub fn decode_stream_response(
    response: reqwest::Response,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>> {
    decode_stream_bytes(response.bytes_stream())
}

/// 流式解码核心：消费字节流，产出事件。
///
/// 与 `decode_stream_response` 拆开是为了可测试——把网络层（`reqwest::Response`）
/// 换成任意字节流，就能在单测里喂 SSE 样本。
pub fn decode_stream_bytes<S>(
    byte_stream: S,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    Box::pin(async_stream::try_stream! {
        futures::pin_mut!(byte_stream);
        let mut buffer = String::new();
        // delta 仅用于实时渲染；终态以 `response.completed` 携带的完整对象为准。
        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.map_err(AdapterError::Transport)?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            for section in sse::drain_sections(&mut buffer) {
                let Some(data) = sse::data_payload(&section) else {
                    continue;
                };
                if data.is_empty() || data == "[DONE]" {
                    // Responses 不发 [DONE]，但容忍它以防中间层补发。
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
                    dto::StreamEvent::ReasoningTextDelta { delta } => {
                        if !delta.is_empty() {
                            yield AdapterEvent::ReasoningDelta(delta);
                        }
                    }
                    dto::StreamEvent::Completed { response } => {
                        yield AdapterEvent::Finished(decode_output(&response));
                        return;
                    }
                    dto::StreamEvent::Incomplete { response } => {
                        // 截断也是合法终态，照常产出 Finished（finish_reason = Length）。
                        yield AdapterEvent::Finished(decode_output(&response));
                        return;
                    }
                    dto::StreamEvent::Failed { response } => {
                        let detail = response
                            .error
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "response.failed".to_owned());
                        Err(AdapterError::Decode(format!("response failed: {detail}")))?;
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
        ModelRequest { messages, tools }
    }

    fn body(config: &ModelConfig, messages: &[message::Message]) -> Value {
        let tools: Vec<&tool::ToolDefinition> = vec![];
        encode_request(config, &request(messages, &tools))
            .expect("编码应成功")
            .body
    }

    #[test]
    fn system_goes_to_instructions_not_input() {
        let messages = vec![
            message::Message::System {
                content: "你是助手".into(),
            },
            message::Message::User {
                content: "你好".into(),
            },
        ];
        let body = body(&config(), &messages);
        assert_eq!(body["instructions"], serde_json::json!("你是助手"));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1, "system 不应进入 input");
        assert_eq!(input[0]["role"], serde_json::json!("user"));
    }

    #[test]
    fn assistant_with_text_and_tool_calls_expands_to_siblings() {
        let messages = vec![message::Message::Assistant {
            content: Some("我来查一下".into()),
            reasoning_content: Some("思考".into()),
            tool_calls: vec![message::ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                arguments: "{\"command\":\"ls\"}".into(),
            }],
        }];
        let body = body(&config(), &messages);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 2, "文本与 function_call 应是兄弟 item");
        assert_eq!(input[0]["type"], serde_json::json!("message"));
        assert_eq!(input[0]["content"][0]["type"], serde_json::json!("output_text"));
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
        assert_eq!(
            body["reasoning"],
            serde_json::json!({ "effort": "high" })
        );
        assert_eq!(body["max_output_tokens"], serde_json::json!(2048));
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["temperature"], serde_json::json!(0.3));
    }

    #[test]
    fn tools_lack_function_wrapper() {
        let definition = tool::ToolDefinition {
            name: "bash".into(),
            description: "执行命令".into(),
            parameters: serde_json::json!({ "type": "object" }),
        };
        let tools = vec![&definition];
        let body = encode_request(&config(), &request(&[], &tools))
            .unwrap()
            .body;
        let encoded = &body["tools"][0];
        assert_eq!(encoded["type"], serde_json::json!("function"));
        assert_eq!(encoded["name"], serde_json::json!("bash"));
        assert!(encoded.get("function").is_none(), "不应有 function 包装层");
    }

    #[test]
    fn decodes_reasoning_message_and_function_call_together() {
        let body = serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                { "type": "reasoning", "content": [{ "type": "reasoning_text", "text": "先想" }] },
                { "type": "message", "role": "assistant",
                  "content": [{ "type": "output_text", "text": "答案" }] },
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
                assert_eq!(content.as_deref(), Some("答案"));
                assert_eq!(reasoning_content.as_deref(), Some("先想"));
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "fc1");
            }
            other => panic!("应为 assistant，实际: {other:?}"),
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
                  "content": [{ "type": "output_text", "text": "被截断" }] }
            ]
        });
        let response = decode_response(body).unwrap();
        assert_eq!(response.finish_reason, ModelFinishReason::Length);
    }

    #[test]
    fn missing_cached_tokens_stays_none() {
        // 未上报 cached_tokens 时不能当 0：cache_hit_rate 应返回 None。
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
                { "type": "message", "role": "assistant", "content": "纯字符串" }
            ]
        });
        let response = decode_response(body).unwrap();
        match response.message {
            message::Message::Assistant { content, .. } => {
                assert_eq!(content.as_deref(), Some("纯字符串"));
            }
            other => panic!("应为 assistant，实际: {other:?}"),
        }
        assert_eq!(response.finish_reason, ModelFinishReason::Stop);
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
        // 关键点：没有 `data: [DONE]`，靠 response.completed 收尾。
        let chunks = vec![
            "event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":0}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
            "event: response.reasoning_text.delta\ndata: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"think\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}]}],\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n",
        ];
        let events = collect_stream(chunks).await;

        assert_eq!(events.len(), 3, "两个 delta + 一个 Finished");
        assert!(matches!(&events[0], AdapterEvent::ContentDelta(d) if d == "Hello"));
        assert!(matches!(&events[1], AdapterEvent::ReasoningDelta(d) if d == "think"));
        match &events[2] {
            AdapterEvent::Finished(response) => {
                assert_eq!(response.finish_reason, ModelFinishReason::Stop);
                match &response.message {
                    message::Message::Assistant { content, .. } => {
                        // 终态以 response.completed 的完整 output 为准
                        assert_eq!(content.as_deref(), Some("Hello world"));
                    }
                    other => panic!("应为 assistant，实际: {other:?}"),
                }
            }
            other => panic!("最后一个事件应为 Finished，实际: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_handles_split_chunks() {
        // 一个事件被拆到两个 chunk 里，分帧要能正确拼接。
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
        let chunks = vec![
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"boom\"}}}\n\n",
        ];
        use futures::StreamExt;
        let stream = futures::stream::iter(vec![Ok::<_, reqwest::Error>(
            bytes::Bytes::from_static(chunks[0].as_bytes()),
        )]);
        let mut decoded = decode_stream_bytes(stream);
        let event = decoded.next().await.expect("应有一个事件");
        let error = event.expect_err("failed 应转为错误");
        assert!(error.to_string().contains("boom"), "错误应携带详情: {error}");
    }

    /// 真实网络端到端测试（默认 ignored）。
    ///
    /// 需要环境变量：
    /// - `DEEPSEEK_API_KEY`（必填，缺失即跳过并返回）
    /// - `SHIRLEY_RESPONSES_BASE_URL`（可选，默认 `https://api.deepseek.com/responses`）
    /// - `SHIRLEY_RESPONSES_MODEL`（可选，默认 `deepseek-flash`）
    ///
    /// 跑法：
    /// ```sh
    /// cargo test -p shirley-agent-sdk --lib responses::tests::live -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "真实网络调用，需 DEEPSEEK_API_KEY"]
    async fn live_responses_round_trip() {
        let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
            eprintln!("跳过：未设置 DEEPSEEK_API_KEY");
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
        };

        let client = reqwest::Client::new();
        let mut stream = crate::adapter::invoke(&client, &config, request).await;
        use futures::StreamExt;

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut finished = None;
        while let Some(event) = stream.next().await {
            match event.expect("流式事件不应报错") {
                AdapterEvent::ContentDelta(delta) => content.push_str(&delta),
                AdapterEvent::ReasoningDelta(delta) => reasoning.push_str(&delta),
                AdapterEvent::Finished(response) => {
                    finished = Some(response);
                    break;
                }
            }
        }

        let finished = finished.expect("流应产出 Finished");
        eprintln!("content   = {content:?}");
        eprintln!("reasoning = {} chars", reasoning.len());
        eprintln!("finish    = {:?}", finished.finish_reason);
        eprintln!("usage     = {:?}", finished.usage);

        assert!(
            !content.trim().is_empty(),
            "应拿到非空文本输出，实际: {content:?}"
        );
        assert!(
            finished.usage.input_tokens > 0,
            "usage.input_tokens 应 > 0"
        );
    }

    /// 真实网络的**工具往返**测试（默认 ignored）：第一轮模型发起 function_call，
    /// 我们回传 function_call_output，第二轮拿到最终文本。
    ///
    /// 这一步专门验证请求编码的 `function_call` / `function_call_output`
    /// 与 `call_id` 配对是否被服务端接受。
    #[tokio::test]
    #[ignore = "真实网络调用，需 DEEPSEEK_API_KEY"]
    async fn live_responses_tool_round_trip() {
        let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
            eprintln!("跳过：未设置 DEEPSEEK_API_KEY");
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
            description: "查询城市天气".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
                "additionalProperties": false
            }),
        };
        let tools = vec![&tool_definition];
        let client = reqwest::Client::new();

        // 第一轮：期望模型发起 function_call。
        let first_messages = vec![
            message::Message::System {
                content: "你是助手。".into(),
            },
            message::Message::User {
                content: "北京现在天气如何？用工具查。".into(),
            },
        ];
        let first = run_once(&client, &config, &first_messages, &tools).await;
        let (call_id, call_name) = match &first.message {
            message::Message::Assistant { tool_calls, .. } => {
                let call = tool_calls.first().expect("应发起一次工具调用");
                eprintln!("tool_call = {} / {}", call.id, call.name);
                (call.id.clone(), call.name.clone())
            }
            other => panic!("第一轮应为含工具调用的 assistant，实际: {other:?}"),
        };
        assert_eq!(first.finish_reason, ModelFinishReason::ToolCalls);
        assert_eq!(call_name, "get_weather");

        // 第二轮：回传 function_call_output。
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

    /// 跑一次非流式请求，返回唯一一个 Finished。
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

    /// 真实网络的**流式工具调用**测试（默认 ignored）。
    ///
    /// 锁死一个关键设计赌注：终态 `response.completed` 携带完整 `output`
    /// （含 `function_call`），所以我们无需聚合 `function_call_arguments.delta`。
    #[tokio::test]
    #[ignore = "真实网络调用，需 DEEPSEEK_API_KEY"]
    async fn live_responses_streaming_tool_call() {
        let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
            eprintln!("跳过：未设置 DEEPSEEK_API_KEY");
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
        eprintln!("finish    = {:?}", finished.finish_reason);
        match &finished.message {
            message::Message::Assistant { tool_calls, .. } => {
                let call = tool_calls.first().expect("终态应含 function_call");
                eprintln!("tool_call = {} / {} / {}", call.id, call.name, call.arguments);
                assert_eq!(call.name, "get_weather");
                assert!(!call.id.is_empty(), "call_id 不应为空");
                assert!(call.arguments.contains("city"), "arguments 应为完整 JSON");
            }
            other => panic!("应为含工具调用的 assistant，实际: {other:?}"),
        }
        assert_eq!(finished.finish_reason, ModelFinishReason::ToolCalls);
        assert!(!reasoning.is_empty(), "流式应产出 reasoning delta");
    }
}
