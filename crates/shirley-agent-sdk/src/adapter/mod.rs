mod chat_completions;
mod responses;
mod sse;

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

#[derive(bon::Builder)]
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
type StreamDecoder = fn(reqwest::Response) -> Pin<
    Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>,
>;

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
        other => Err(AdapterError::UnsupportedProtocol {
            protocol: format!("{other:?}"),
        }),
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
    let body = response.text().await.map_err(AdapterError::Transport)?;
    Err(AdapterError::Http {
        status: status.as_u16(),
        body,
    })
}

pub async fn invoke<'a>(
    client: &'a reqwest::Client,
    config: &'a ModelConfig,
    input: ModelRequest<'a>,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send + 'a>> {
    Box::pin(async_stream::try_stream! {
        let (encode, decode, decode_stream) = codec(&config.protocol)?;
        let prepared = encode(config, &input)?;
        let response = client
            .post(&prepared.url)
            .headers(prepared.headers)
            .json(&prepared.body)
            .send()
            .await
            .map_err(AdapterError::Transport)?;

        // 状态检查放在这里，错误体与状态码一起保留下来。
        let response = ensure_success(response).await?;

        if !config.stream {
            let text = response.text().await.map_err(AdapterError::Transport)?;
            let body = serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|e| AdapterError::Decode(format!("response body is not valid JSON: {e}")))?;

            let msg = decode(body)?;
            yield AdapterEvent::Finished(msg)
        } else {
            let mut stream = decode_stream(response);
            while let Some(event) = stream.next().await {
                yield event?;
            }
        }
    })
}
