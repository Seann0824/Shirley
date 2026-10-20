mod anthropic_messages;
mod chat_completions;
mod content;
mod responses;
mod sse;
mod tool_calling;

pub use tool_calling::ToolCallingMode;

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue};
use std::{format, pin::Pin};

use crate::error::{ErrorKind, SdkError};
use crate::{message, tool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProtocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

#[derive(bon::Builder, Clone, Debug)]
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
    /// Maximum silence while waiting for the next decoded event, including headers and
    /// non-streaming bodies. Healthy long streams reset this deadline on every event.
    #[builder(default = std::time::Duration::from_secs(180))]
    pub request_timeout: std::time::Duration,
    #[builder(default)]
    pub thinking: bool,
    #[builder(into)]
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u32>,
    pub context_window_tokens: Option<u64>,
    /// Whether the configured model accepts image input. Defaults to `false`
    /// (deny by default): the send-time guard rejects a request that carries an
    /// image unless the application has explicitly opted in.
    #[builder(default)]
    pub supports_images: bool,
    /// `tool_choice` is passed through as-is (`"auto"` / `"none"` / `"required"` or an object).
    ///
    /// Uses [`serde_json::Value`] rather than an enum: providers vary in shape, and an enum would
    /// repeatedly break the API. `None` means the field is not sent (use the endpoint default).
    pub tool_choice: Option<serde_json::Value>,
    /// An escape hatch to override / append to the request body.
    ///
    /// Vendor-private fields (`enable_thinking` / `thinking_budget` / `{type:"enabled"}` …) vary
    /// wildly, and adding a branch for each is a bottomless pit. The app puts the fields it wants
    /// to change here, and after building the standard fields the SDK does a **shallow merge**:
    /// app keys override standard keys, and **a `null` value deletes that key** (so the app can
    /// fully remove a field the SDK writes by default). `None` means no override at all.
    #[builder(into)]
    pub extra_body: Option<serde_json::Value>,
    /// Tool transport mode: `Native` (default, structured `tools` / `tool_calls`) or
    /// `Content(protocol)` (a text protocol, see [`tool_calling`]).
    ///
    /// **Orthogonal to** [`ModelProtocol`]: content combines with any underlying protocol; it is
    /// "how tools are transmitted", not "how the conversation is encoded". In content mode
    /// `invoke()` will: render `Message::Tool` as text before encoding, remove the native `tools`
    /// field after encoding, and parse text back into `tool_calls` after decoding — the three
    /// codecs unchanged by a single line (the difference is contained in this layer).
    #[builder(default)]
    pub tool_calling: tool_calling::ToolCallingMode,
}

#[derive(Debug)]
pub enum AdapterEvent {
    Retrying {
        attempt: usize,
        reason: String,
    },
    ReasoningDelta(String),
    ContentDelta(String),
    /// The request carried images but they were silently dropped because the model cannot
    /// read them (declared unsupported, or the provider rejected image input). The request is
    /// re-sent as text; the UI turns this into a neutral, human-readable notice. This never
    /// surfaces a "does it support images" capability question to the user.
    ImagesDropped {
        reason: String,
    },
    Finished(ModelResponse),
}

/// Adapter-layer error.
///
/// Deliberately keeps the HTTP status code and the underlying error object: retry, degradation,
/// and cancellation decisions all depend on "which kind of error this is" (see
/// `docs/runtime-hardening.md` section 2); flattening the status code into a string throws away
/// the basis for the decision.
///
/// Display format is uniformly `[prefix]: detail`, consistent with `ToolError` / `SandboxError` /
/// `WorkspaceError`; the prefix stays next to the variant and is not derived from [`ErrorKind`].
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("[transport failure]: model made no progress for {0:?}")]
    Timeout(std::time::Duration),
    #[error("[transport failure]: model stream ended without a completion event")]
    IncompleteStream,
    /// Network-layer failure: cannot connect, connection reset, response body read interrupted, etc.
    #[error("[transport failure]: {0}")]
    Transport(#[source] reqwest::Error),

    /// The server returned a non-2xx. The status code must be kept; it is the sole basis for retryability.
    #[error("[HTTP {status}]: {body}")]
    Http { status: u16, body: String },

    /// Response body parsing failed (JSON structure doesn't match the protocol, missing fields, etc.).
    #[error("[decode failure]: {0}")]
    Decode(String),

    /// Request encoding failed (e.g. the API key can't form a valid header).
    #[error("[encode failure]: {0}")]
    Encode(String),

    /// The requested protocol is not implemented. Never express "not implemented" with a panic.
    #[error("[unsupported protocol]: {protocol}")]
    UnsupportedProtocol { protocol: String },
}

impl SdkError for AdapterError {
    fn kind(&self) -> ErrorKind {
        match self {
            // Network layer: transient, retryable.
            Self::Transport(_) | Self::Timeout(_) | Self::IncompleteStream => ErrorKind::Transport,
            // The status code decides the semantics: 429 rate limit, 5xx server, the rest are bad requests.
            Self::Http { status, .. } => match status {
                429 => ErrorKind::RateLimited,
                500..=599 => ErrorKind::ServerError,
                _ => ErrorKind::BadRequest,
            },
            // A parse/encode failure won't improve by retrying the same input.
            Self::Decode(_) | Self::Encode(_) => ErrorKind::Internal,
            Self::UnsupportedProtocol { .. } => ErrorKind::Unsupported,
        }
    }
}

#[derive(Clone, Copy)]
pub struct ModelRequest<'a> {
    pub messages: &'a [message::Message],
    pub tools: &'a [&'a tool::ToolDefinition],
    /// Optional application conversation key for a local stateful proxy.
    pub session_id: Option<&'a str>,
}

/// Local proxies may keep a browser conversation for one application session. Never send this
/// app identity to remote model providers.
pub(crate) fn attach_local_session_header(
    headers: &mut HeaderMap,
    base_url: &str,
    session_id: Option<&str>,
) -> Result<(), AdapterError> {
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let is_loopback = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| {
            url.host_str().map(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            })
        })
        .unwrap_or(false);
    if !is_loopback {
        return Ok(());
    }
    let value = HeaderValue::from_str(session_id).map_err(|error| {
        AdapterError::Encode(format!("session id is not a valid header value: {error}"))
    })?;
    headers.insert("x-shirley-session-id", value);
    Ok(())
}

#[cfg(test)]
mod session_header_tests {
    use super::*;

    #[test]
    fn sends_session_identity_only_to_loopback_endpoints() {
        let mut local = HeaderMap::new();
        attach_local_session_header(
            &mut local,
            "http://127.0.0.1:8788/v1/chat/completions",
            Some("session-1"),
        )
        .unwrap();
        assert_eq!(local.get("x-shirley-session-id").unwrap(), "session-1");

        let mut remote = HeaderMap::new();
        attach_local_session_header(
            &mut remote,
            "https://api.example.com/v1/chat/completions",
            Some("session-1"),
        )
        .unwrap();
        assert!(remote.get("x-shirley-session-id").is_none());
    }
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
    /// Parsed from the response and returned with the result, for upper layers to later decide on
    /// truncation / tool calls; currently unconsumed.
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
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
/// Streaming decode. Also folded into the codec, avoiding "one more hard branch per protocol" in `invoke`.
type StreamDecoder =
    fn(
        reqwest::Response,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send>>;

/// Get the codec triple by protocol: request encoding / non-streaming decode / streaming decode.
///
/// Every protocol is implemented, so this is a total mapping with no failure case; an unknown
/// protocol cannot reach here (the enum is closed).
fn codec(protocol: ModelProtocol) -> (Encoder, Decoder, StreamDecoder) {
    match protocol {
        ModelProtocol::ChatCompletions => (
            chat_completions::encode_request,
            chat_completions::decode_response,
            chat_completions::decode_stream_response,
        ),
        ModelProtocol::Responses => (
            responses::encode_request,
            responses::decode_response,
            responses::decode_stream_response,
        ),
        ModelProtocol::AnthropicMessages => (
            anthropic_messages::encode_request,
            anthropic_messages::decode_response,
            anthropic_messages::decode_stream_response,
        ),
    }
}

/// Check the HTTP status: on non-2xx, read the error body and return a structured error.
///
/// Pulled out separately for two reasons:
/// 1. `reqwest::Response` can only be consumed once, so "check status, then read body" must
///    happen in one place;
/// 2. both streaming and non-streaming need this logic, and both must keep the status code (the
///    sole basis for the retry decision).
async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response, AdapterError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    // A known HTTP status must not be overwritten by a failure to read the error body, or a 400
    // would also become a retryable error.
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| error.to_string());
    Err(AdapterError::Http {
        status: status.as_u16(),
        body,
    })
}

/// At most 5 retries after the first request fails, for at most 6 requests total.
const MAX_RETRIES: usize = 5;
/// Backoff base: before the n-th retry, wait `BASE_BACKOFF_MS * 2^(n-1)`, capped at `MAX_BACKOFF_MS`.
const BASE_BACKOFF_MS: u64 = 500;
const MAX_BACKOFF_MS: u64 = 8_000;

/// A single request and decode; the retry boundary is decided by invoke based on whether an event
/// has already been emitted.
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
            let mut finished = false;
            while let Some(event) = stream.next().await {
                let event = event?;
                finished = matches!(&event, AdapterEvent::Finished(_));
                yield event;
                if finished { break; }
            }
            if !finished { Err(AdapterError::IncompleteStream)?; }
        } else {
            let text = response.text().await.map_err(AdapterError::Transport)?;
            let body = serde_json::from_str(&text)
                .map_err(|error| AdapterError::Decode(format!("response body is not valid JSON: {error}")))?;
            yield AdapterEvent::Finished(decode(body)?);
        }
    })
}

pub fn invoke<'a>(
    client: &'a reqwest::Client,
    config: &'a ModelConfig,
    input: ModelRequest<'a>,
) -> Pin<Box<dyn futures::Stream<Item = Result<AdapterEvent, AdapterError>> + Send + 'a>> {
    Box::pin(async_stream::try_stream! {
        let (encode, decode, decode_stream) = codec(config.protocol);

        // Optimistic image handling (user-invisible). Images are accepted by default; the
        // only reason to drop them up front is an explicit "this model has no image support"
        // declaration (internal escape hatch). In that case we silently degrade instead of
        // failing: the user keeps the attach entry and just sees the answer, with a neutral
        // notice. A provider that *rejects* image input at request time is handled below by
        // the same strip-and-resend path.
        let mut messages: std::borrow::Cow<'_, [message::Message]> =
            std::borrow::Cow::Borrowed(input.messages);
        if !config.supports_images && content::messages_have_images(messages.as_ref()) {
            messages = std::borrow::Cow::Owned(content::strip_images(messages.as_ref()));
            yield AdapterEvent::ImagesDropped {
                reason: "the configured model does not accept image input".to_owned(),
            };
        }

        // Content tool protocol (see `tool_calling`): the protocol instance is built internally
        // from the mode, invisible to the outside. Encoding view: the tool catalog is injected
        // into system, history is re-rendered into the protocol's original text, Tool results are
        // turned back into feedback text; meanwhile pass empty tools and clear tool_choice, so no
        // native tool fields are generated at the source.
        let protocol = config.tool_calling.protocol();
        let no_tools: [&tool::ToolDefinition; 0] = [];

        // Build the encoded request from the current message view. A closure (not one-shot code)
        // because the image-degradation retry below needs to re-encode with images stripped.
        let build_prepared = |messages: &[message::Message]| -> Result<PreparedRequest, AdapterError> {
            let content_view: Option<Vec<message::Message>> = protocol.as_deref().map(|protocol| {
                tool_calling::encode_messages_for_content(messages, input.tools, protocol)
            });
            let content_config;
            let encode_config = if protocol.is_some() {
                // Content mode: clear the native tool fields. `tool_choice` is set to None directly;
                // `parallel_tool_calls` isn't a standard field, so it's set to `null` via `extra_body`
                // to delete it (in the shallow merge `null` means delete the key), avoiding residual
                // native tool behavior at the endpoint.
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
                messages: content_view.as_deref().unwrap_or(messages),
                tools: if protocol.is_some() { &no_tools } else { input.tools },
                session_id: input.session_id,
            };
            encode(encode_config, &encode_input)
        };

        let mut retries = 0;
        let mut emitted = false;
        let mut image_retry_done = false;
        'attempt: loop {
            // Re-encode each attempt from the current message view. On an image rejection we
            // strip the images, `continue 'attempt`, and this line rebuilds the text-only body
            // (the previous `stream`/`prepared` are dropped by the `continue`).
            let prepared = build_prepared(messages.as_ref())?;
            let mut stream = invoke_prepared(client, &prepared, config.stream, decode, decode_stream);
            // Content mode: build a **stream parser** that translates raw protocol text deltas
            // into visible deltas (thinking → ReasoningDelta, description → ContentDelta, tool
            // blocks suppressed). A protocol with no stream parser swallows everything by default
            // (nothing leaks); the final state is still given by Finished.
            let mut stream_parser = protocol.as_deref().map(tool_calling::ContentToolProtocol::stream_parser);
            while let Some(event) = tokio::time::timeout(config.request_timeout, stream.next())
                .await.unwrap_or_else(|_| Some(Err(AdapterError::Timeout(config.request_timeout)))) {
                let event = match event {
                    Ok(event) => event,
                    Err(error) if !emitted && retries < MAX_RETRIES && error.is_retryable() => {
                        let backoff = (BASE_BACKOFF_MS << retries).min(MAX_BACKOFF_MS);
                        retries += 1;
                        yield AdapterEvent::Retrying { attempt: retries, reason: error.to_string() };
                        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                        continue 'attempt;
                    }
                    // The provider rejected the request because of the images: drop them and
                    // silently re-send as text, exactly once. This is the "optimistic default"
                    // payoff — no capability prompt, no failure surfaced, the user just gets the
                    // answer. Only happens before any delta is emitted, so nothing to unwind.
                    Err(error)
                        if !emitted
                            && !image_retry_done
                            && content::messages_have_images(messages.as_ref())
                            && looks_like_image_rejection(&error) =>
                    {
                        image_retry_done = true;
                        messages = std::borrow::Cow::Owned(content::strip_images(messages.as_ref()));
                        yield AdapterEvent::ImagesDropped { reason: error.to_string() };
                        continue 'attempt;
                    }
                    Err(error) => Err(error)?,
                };
                let Some(parser) = stream_parser.as_mut() else {
                    // Native mode: pass through as-is.
                    emitted = true;
                    yield event;
                    continue;
                };
                match event {
                    AdapterEvent::Retrying { attempt, reason } => {
                        yield AdapterEvent::Retrying { attempt, reason };
                    }
                    // `invoke_prepared` never emits this (only `invoke` synthesizes it); ignore.
                    AdapterEvent::ImagesDropped { .. } => {}
                    AdapterEvent::ContentDelta(text) => {
                        for chunk in parser.push(&text) {
                            emitted = true;
                            yield visible_chunk_event(chunk);
                        }
                    }
                    // The reasoning deltas reported by the decoder pass through as-is (protocol-agnostic).
                    AdapterEvent::ReasoningDelta(text) => {
                        emitted = true;
                        yield AdapterEvent::ReasoningDelta(text);
                    }
                    AdapterEvent::Finished(mut response) => {
                        // Flush any residual tail deltas, then parse out structured `tool_calls`.
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

/// Heuristic: does this HTTP error mean "this endpoint/model cannot take images"?
///
/// Providers word it many ways ("image input is not supported", "unknown modality", "vision
/// not available", content-type rejections), so we look at the status class plus a small set
/// of keywords in the body. Deliberately conservative: only client errors that mention image /
/// vision / modality are treated as an image rejection — anything else propagates unchanged.
fn looks_like_image_rejection(error: &AdapterError) -> bool {
    let AdapterError::Http { status, body } = error else {
        return false;
    };
    if !matches!(status, 400 | 415 | 422) {
        return false;
    }
    let body = body.to_ascii_lowercase();
    ["image", "vision", "modality", "multimodal", "image_url"]
        .iter()
        .any(|keyword| body.contains(keyword))
}

/// Layer a `parallel_tool_calls: null` (delete semantics) onto `extra_body`, used in content mode
/// to clear the native parallel-tool switch the endpoint may write. A `None` input also produces
/// an object containing only that delete key.
fn remove_parallel_tool_calls(extra: Option<serde_json::Value>) -> serde_json::Value {
    let mut object = match extra {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    object.insert("parallel_tool_calls".to_string(), serde_json::Value::Null);
    serde_json::Value::Object(object)
}

/// Map the visible deltas emitted by the stream parser into outward events (unified abstraction:
/// the UI only knows `AdapterEvent`).
fn visible_chunk_event(chunk: tool_calling::VisibleChunk) -> AdapterEvent {
    match chunk {
        tool_calling::VisibleChunk::Thinking(text) => AdapterEvent::ReasoningDelta(text),
        tool_calling::VisibleChunk::Content(text) => AdapterEvent::ContentDelta(text),
    }
}
