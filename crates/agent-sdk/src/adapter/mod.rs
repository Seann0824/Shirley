mod chat_completions;

use futures::StreamExt;
use reqwest::header::HeaderMap;
use serde::Serialize;
use std::{format, pin::Pin, time::Duration, todo};

use crate::{AgentError::AdapterError, message, tool};

pub enum ModelProtocol {
    ChatCompletions,
    Responses,
    AnyhtopicMessages,
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
    #[builder(default = Duration::from_secs(60))]
    pub request_timeout: Duration,
    #[builder(default)]
    pub stream: bool,
    #[builder(default)]
    pub thinking: bool,
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u32>,
    pub context_window_tokens: Option<u64>,
}

pub type ModelError = String;

pub enum AdapterEvent {
    ReasoningDelta(String),
    ContentDelta(String),
    Finished(ModelResponse),
}

#[derive(Debug, Serialize)]
pub enum AdpaterError {
    RequestError(String),
    ResponseError(String),
}

pub struct ModelRequest<'a> {
    pub messages: &'a [message::Message],
    pub tools: &'a [&'a tool::ToolDefinition],
}

#[derive(Debug, PartialEq, Eq)]
pub enum ModelfinishReaon {
    Stop,
    ToolCalls,
    Length,
    Other(String),
}

pub struct ModelResponse {
    pub message: message::Message,
    pub finish_reason: ModelfinishReaon,
    pub usage: message::Usage,
}

pub struct PreparedRequest {
    pub url: String,
    pub headers: HeaderMap,
    pub body: serde_json::Value,
}

type Encoder = fn(&ModelConfig, &ModelRequest<'_>) -> Result<PreparedRequest, AdpaterError>;
type Decoder = fn(serde_json::Value) -> Result<ModelResponse, AdpaterError>;

fn codec(protocol: &ModelProtocol) -> (Encoder, Decoder) {
    match protocol {
        ModelProtocol::ChatCompletions => (
            chat_completions::encode_request,
            chat_completions::decode_response,
        ),
        _ => todo!(),
    }
}

pub async fn invoke<'a>(
    client: &'a reqwest::Client,
    config: &'a ModelConfig,
    input: ModelRequest<'a>,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdpaterError>> + Send + 'a>> {
    Box::pin(async_stream::try_stream! {
       let (encode, decode) = codec(&config.protocol);
       let prepared = encode(config, &input)?;
       let response = client
           .post(&prepared.url)
           .headers(prepared.headers)
           .json(&prepared.body)
           .send()
           .await
           .map_err(|e| AdpaterError::RequestError(format!("模型请求失败 {}", e.to_string())))?;

       if !config.stream {
           let status = response.status();
           let text = response.text().await.map_err(|e| {
               AdpaterError::ResponseError(format!("非流式响应解析失败: {}", e.to_string()))
           })?;
           if !status.is_success() {
                Err(AdpaterError::ResponseError(format!(
                    "HTTP {status}\n{text}"
                )))?;
           }
           let body = serde_json::from_str::<serde_json::Value>(&text).map_err(|e| AdpaterError::ResponseError(
                format!("JSON 序列化失败: {}", e.to_string())
           ))?;

           let msg = decode(body)?;
           yield AdapterEvent::Finished(msg)
       }

    })
}
