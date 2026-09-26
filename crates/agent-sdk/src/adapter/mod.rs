mod chat_completions;

use std::{time::Duration, todo};

use reqwest::header::HeaderMap;

use crate::{message, tool};

pub enum ModelProtocol {
    ChatCompletions,
    Responses,
    AnyhtopicMessages,
}

#[derive(Default)]
pub struct GenerationConfig {
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u32>,
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
    pub generation: GenerationConfig,
}

pub type ModelError = String;

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

type Encoder = fn(&ModelConfig, &ModelRequest<'_>) -> Result<PreparedRequest, ModelError>;
type Decoder = fn(serde_json::Value) -> Result<ModelResponse, ModelError>;

fn codec(protocol: &ModelProtocol) -> (Encoder, Decoder) {
    match protocol {
        ModelProtocol::ChatCompletions => (
            chat_completions::encode_request,
            chat_completions::decode_response,
        ),
        _ => todo!(),
    }
}

pub async fn invoke(
    client: &reqwest::Client,
    config: &ModelConfig,
    input: ModelRequest<'_>,
) -> Result<ModelResponse, ModelError> {
    let (encode, decode) = codec(&config.protocol);

    let prepared = encode(config, &input)?;

    let response = client
        .post(&prepared.url)
        .headers(prepared.headers)
        .json(&prepared.body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;

    if !status.is_success() {
        return Err(format!("HTTP {status}\n{text}"));
    }

    let body = serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string())?;

    decode(body)
}
