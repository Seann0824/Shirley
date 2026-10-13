mod anthropic_messages;
mod chat_completions;
mod responses;
mod sse;
mod tool_calling;

pub use tool_calling::ToolCallingMode;

use futures::StreamExt;
use reqwest::header::HeaderMap;
use std::{format, pin::Pin};

use crate::error::{ErrorKind, SdkError};
use crate::{message, tool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProtocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

#[derive(bon::Builder, Clone)]
pub struct ModelConfig {
    pub protocol: ModelProtocol,
    #[builder(into)]
    pub base_url: String,
    #[builder(into)]
    pub model: String,
    #[builder(into)]
    pub api_key: Option<String>,
    #[builder(default)]
    pub stream: bool,
    #[builder(default)]
    pub thinking: bool,
    #[builder(into)]
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u32>,
    pub context_window_tokens: Option<u64>,
    /// `tool_choice` 原样透传（`"auto"` / `"none"` / `"required"` 或对象）。
    ///
    /// 用 [`serde_json::Value`] 而非枚举：各家取值形态不一，枚举会反复破 API。
    /// `None` 表示不发送该字段（沿用端点默认）。
    pub tool_choice: Option<serde_json::Value>,
    /// 覆盖 / 追加请求体的逃生口。
    ///
    /// 各厂商私有字段（`enable_thinking` / `thinking_budget` / `{type:"enabled"}` …）
    /// 差异极大，逐个加分支是无底洞。应用把要改的字段放这里，SDK 在标准字段
    /// 构造完成后做一次**浅合并**：应用键覆盖标准键，**值为 `null` 表示删除该键**
    /// （这样应用能彻底移除 SDK 默认写入的字段）。`None` 表示不做任何覆盖。
    #[builder(into)]
    pub extra_body: Option<serde_json::Value>,
    /// 工具传输模式：`Native`（默认，结构化 `tools` / `tool_calls`）或
    /// `Content(协议)`（文本协议，见 [`tool_calling`]）。
    ///
    /// **正交于** [`ModelProtocol`]：content 可与任意底层协议组合，它是"工具怎么传"
    /// 而非"对话怎么编码"。content 模式下 `invoke()` 会：编码前把 `Message::Tool`
    /// 渲染成文本、编码后移除原生 `tools` 字段、解码后把文本解析回 `tool_calls`——
    /// 三个 codec 一行不改（差异收敛在这一层）。
    #[builder(default)]
    pub tool_calling: tool_calling::ToolCallingMode,
}

#[derive(Debug)]
pub enum AdapterEvent {
    ReasoningDelta(String),
    ContentDelta(String),
    Finished(ModelResponse),
}

/// 适配层错误。
///
/// 刻意保留 HTTP 状态码与底层错误对象：重试、降级、取消的决策都依赖
/// "这个错误是哪一类"（见 `docs/runtime-hardening.md` 第二节），
/// 把状态码压成字符串就等于把决策依据丢了。
///
/// 展示格式统一为 `[前缀]: 详情`，与 `ToolError` / `SandboxError` /
/// `WorkspaceError` 一致；前缀留在变体旁，不从 [`ErrorKind`] 推导。
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// 网络层失败：连不上、连接被重置、读取响应体中断等。
    #[error("[transport failure]: {0}")]
    Transport(#[source] reqwest::Error),

    /// 服务端返回了非 2xx。状态码必须保留，它是可重试性的唯一依据。
    #[error("[HTTP {status}]: {body}")]
    Http { status: u16, body: String },

    /// 响应体解析失败（JSON 结构不符合协议、缺字段等）。
    #[error("[decode failure]: {0}")]
    Decode(String),

    /// 请求编码失败（例如 API Key 无法构成合法请求头）。
    #[error("[encode failure]: {0}")]
    Encode(String),

    /// 请求的协议还没有实现。绝不用 panic 表达"未实现"。
    #[error("[unsupported protocol]: {protocol}")]
    UnsupportedProtocol { protocol: String },
}

impl SdkError for AdapterError {
    fn kind(&self) -> ErrorKind {
        match self {
            // 网络层：瞬时，可重试。
            Self::Transport(_) => ErrorKind::Transport,
            // 状态码决定语义：429 限流、5xx 服务端、其余是请求本身有问题。
            Self::Http { status, .. } => match status {
                429 => ErrorKind::RateLimited,
                500..=599 => ErrorKind::ServerError,
                _ => ErrorKind::BadRequest,
            },
            // 解析/编码失败重试同一份输入不会变好。
            Self::Decode(_) | Self::Encode(_) => ErrorKind::Internal,
            Self::UnsupportedProtocol { .. } => ErrorKind::Unsupported,
        }
    }
}

pub struct ModelRequest<'a> {
    pub messages: &'a [message::Message],
    pub tools: &'a [&'a tool::ToolDefinition],
}

#[derive(Debug, PartialEq, Eq)]
pub enum ModelFinishReason {
    Stop,
    ToolCalls,
    Length,
    Other(String),
}

#[derive(Debug)]
pub struct ModelResponse {
    pub message: message::Message,
    /// 解析自响应、随结果返回，供上层将来做截断/工具调用判定；当前未消费。
    #[allow(dead_code)]
    pub finish_reason: ModelFinishReason,
    pub usage: message::Usage,
}

pub struct PreparedRequest {
    pub url: String,
    pub headers: HeaderMap,
    pub body: serde_json::Value,
}

type Encoder = fn(&ModelConfig, &ModelRequest<'_>) -> Result<PreparedRequest, AdapterError>;
type Decoder = fn(serde_json::Value) -> Result<ModelResponse, AdapterError>;
/// 流式解码。也纳入 codec，避免 `invoke` 里出现"每加一个协议多一个硬分支"。
type StreamDecoder =
    fn(
        reqwest::Response,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>>;

/// 按协议取编解码函数三件套：请求编码 / 非流式解码 / 流式解码。
///
/// 未实现的协议返回 `Err` 而不是 panic：切换协议是运行时行为，
/// 不能让它把宿主进程带走（见 `docs/runtime-hardening.md` 第七节）。
fn codec(protocol: &ModelProtocol) -> Result<(Encoder, Decoder, StreamDecoder), AdapterError> {
    match protocol {
        ModelProtocol::ChatCompletions => Ok((
            chat_completions::encode_request,
            chat_completions::decode_response,
            chat_completions::decode_stream_response,
        )),
        ModelProtocol::Responses => Ok((
            responses::encode_request,
            responses::decode_response,
            responses::decode_stream_response,
        )),
        ModelProtocol::AnthropicMessages => Ok((
            anthropic_messages::encode_request,
            anthropic_messages::decode_response,
            anthropic_messages::decode_stream_response,
        )),
    }
}

/// 检查 HTTP 状态：非 2xx 时读出错误体并返回结构化错误。
///
/// 单独抽出来有两个原因：
/// 1. `reqwest::Response` 只能被消费一次，"先检查状态、后读 body" 必须在同一处完成；
/// 2. 流式与非流式都要这段逻辑，且都必须保留状态码（重试决策的唯一依据）。
async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response, AdapterError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    // 已知的 HTTP 状态不能被读取错误体失败覆盖，否则 400 也会变成可重试错误。
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| error.to_string());
    Err(AdapterError::Http {
        status: status.as_u16(),
        body,
    })
}

/// 首次请求失败后最多重试 5 次，共最多 6 次请求。
const MAX_RETRIES: usize = 5;
/// 退避基数：第 n 次重试前等待 `BASE_BACKOFF_MS * 2^(n-1)`，封顶 `MAX_BACKOFF_MS`。
const BASE_BACKOFF_MS: u64 = 500;
const MAX_BACKOFF_MS: u64 = 8_000;

/// 单次请求与解码；重试边界由 invoke 按是否已发出事件决定。
fn invoke_prepared<'a>(
    client: &'a reqwest::Client,
    prepared: &'a PreparedRequest,
    streaming: bool,
    decode: Decoder,
    decode_stream: StreamDecoder,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send + 'a>> {
    Box::pin(async_stream::try_stream! {
        let response = client
            .post(&prepared.url)
            .headers(prepared.headers.clone())
            .json(&prepared.body)
            .send()
            .await.map_err(AdapterError::Transport)?;
        let response = ensure_success(response).await?;
        if streaming {
            let mut stream = decode_stream(response);
            while let Some(event) = stream.next().await {
                yield event?;
            }
        } else {
            let text = response.text().await.map_err(AdapterError::Transport)?;
            let body = serde_json::from_str(&text)
                .map_err(|error| AdapterError::Decode(format!("response body is not valid JSON: {error}")))?;
            yield AdapterEvent::Finished(decode(body)?);
        }
    })
}

pub async fn invoke<'a>(
    client: &'a reqwest::Client,
    config: &'a ModelConfig,
    input: ModelRequest<'a>,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send + 'a>> {
    Box::pin(async_stream::try_stream! {
        let (encode, decode, decode_stream) = codec(&config.protocol)?;

        // content 工具协议（见 `tool_calling`）：协议实例由模式在内部构造，外部不感知。
        // 编码视图：工具目录注入 system、历史重渲染成协议原文、Tool 结果转回灌文本；
        // 同时传空 tools、清掉 tool_choice，从源头不生成原生工具字段。
        let protocol = config.tool_calling.protocol();
        let content_view: Option<Vec<message::Message>> = protocol.as_deref().map(|protocol| {
            tool_calling::encode_messages_for_content(input.messages, input.tools, protocol)
        });
        let no_tools: [&tool::ToolDefinition; 0] = [];
        let content_config;
        let encode_config = if protocol.is_some() {
            // content 模式：清掉原生工具字段。`tool_choice` 直接置 None；
            // `parallel_tool_calls` 不在标准字段里，经 `extra_body` 置 `null` 删除
            // （浅合并里 `null` 表示删除该键），避免端点残留原生工具行为。
            content_config = ModelConfig {
                tool_choice: None,
                extra_body: Some(remove_parallel_tool_calls(config.extra_body.clone())),
                ..config.clone()
            };
            &content_config
        } else {
            config
        };
        let encode_input = ModelRequest {
            messages: content_view.as_deref().unwrap_or(input.messages),
            tools: if protocol.is_some() { &no_tools } else { input.tools },
        };
        let prepared = encode(encode_config, &encode_input)?;
        let mut retries = 0;
        let mut emitted = false;
        'attempt: loop {
            let mut stream = invoke_prepared(client, &prepared, config.stream, decode, decode_stream);
            // content 模式：建一个**流式解析器**，把原始协议文本增量翻译成可见增量
            // （thinking → ReasoningDelta、description → ContentDelta，工具块屏蔽）。
            // 未实现流式解析的协议默认吞掉一切（不外泄），最终态仍由 Finished 给出。
            let mut stream_parser = protocol.as_deref().map(|protocol| protocol.stream_parser());
            while let Some(event) = stream.next().await {
                let event = match event {
                    Ok(event) => event,
                    Err(error) if !emitted && retries < MAX_RETRIES && error.is_retryable() => {
                        let backoff = (BASE_BACKOFF_MS << retries).min(MAX_BACKOFF_MS);
                        retries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                        continue 'attempt;
                    }
                    Err(error) => Err(error)?,
                };
                let Some(parser) = stream_parser.as_mut() else {
                    // native 模式：原样透传。
                    emitted = true;
                    yield event;
                    continue;
                };
                match event {
                    AdapterEvent::ContentDelta(text) => {
                        for chunk in parser.push(&text) {
                            emitted = true;
                            yield visible_chunk_event(chunk);
                        }
                    }
                    // 解码器上报的 reasoning 增量原样透传（协议无关）。
                    AdapterEvent::ReasoningDelta(text) => {
                        emitted = true;
                        yield AdapterEvent::ReasoningDelta(text);
                    }
                    AdapterEvent::Finished(mut response) => {
                        // 冲刷尾部残留增量，再解析出结构化 `tool_calls`。
                        for chunk in parser.finish() {
                            yield visible_chunk_event(chunk);
                        }
                        response.message = tool_calling::parse_content_response(
                            protocol.as_deref().expect("content mode implies a protocol"),
                            response.message,
                        );
                        emitted = true;
                        yield AdapterEvent::Finished(response);
                    }
                }
            }
            break;
        }
    })
}

/// 在 `extra_body` 上叠一个 `parallel_tool_calls: null`（删除语义），用于 content 模式
/// 清掉端点可能写入的原生并行工具开关。`None` 输入也会产出一个只含该删除键的对象。
fn remove_parallel_tool_calls(extra: Option<serde_json::Value>) -> serde_json::Value {
    let mut object = match extra {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    object.insert("parallel_tool_calls".to_string(), serde_json::Value::Null);
    serde_json::Value::Object(object)
}

/// 把流式解析器吐出的可见增量映射成对外事件（统一抽象：UI 只认 `AdapterEvent`）。
fn visible_chunk_event(chunk: tool_calling::VisibleChunk) -> AdapterEvent {
    match chunk {
        tool_calling::VisibleChunk::Thinking(text) => AdapterEvent::ReasoningDelta(text),
        tool_calling::VisibleChunk::Content(text) => AdapterEvent::ContentDelta(text),
    }
}
