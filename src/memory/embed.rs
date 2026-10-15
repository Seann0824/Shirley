//! Semantic vectors: OpenAI-compatible `POST /v1/embeddings` client (`docs/memory.md` 10 / V2.5).
//!
//! **Honest degradation**: this module is only responsible for "how to call an OpenAI-compatible embeddings endpoint". Whether it is enabled
//! is decided by configuration -- without `[memory] embedding_*` configured, [`crate::bootstrap`] will not construct
//! an [`Embedder`], and retrieval degrades to pure BM25 (Chinese already goes through bigrams, which is usable). **Never pretend there is a semantic leg.**
//!
//! Security constraints align with `tools/web_search.rs`: no redirect following, connect / total timeouts, capped response body.
//! No vector database / new crate -- vectors go to a sidecar file (see [`super::vector`]).

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Total timeout for a single request.
pub const EMBED_TIMEOUT: Duration = Duration::from_secs(15);
/// Connect timeout.
pub const EMBED_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum number of texts to embed per request (batched, to be safe).
pub const MAX_BATCH: usize = 64;
/// Error body truncation length.
const MAX_ERROR_BODY: usize = 300;

/// embeddings call error.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// Network / client build failure.
    #[error("embedding transport error: {0}")]
    Transport(String),
    /// Non-2xx response.
    #[error("embedding endpoint returned {status}: {body}")]
    Status { status: u16, body: String },
    /// Response is not the expected JSON structure (e.g. base64 vectors -- this implementation only accepts float arrays).
    #[error("embedding response malformed: {0}")]
    Decode(String),
}

/// embedding endpoint configuration.
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Full address, e.g. `http://127.0.0.1:11434/v1/embeddings`.
    pub endpoint: String,
    /// Key; `None` / empty string is treated as "no auth" (common for local services).
    pub api_key: Option<String>,
    /// Model identifier, e.g. `text-embedding-3-small` / `bge-m3`.
    pub model: String,
}

/// OpenAI-compatible embeddings client (clonable; the inner `reqwest::Client` shares a connection pool).
#[derive(Clone)]
pub struct Embedder {
    client: reqwest::Client,
    config: EmbeddingConfig,
}

impl Embedder {
    /// Construct the client (validates that the endpoint is non-empty).
    pub fn new(config: EmbeddingConfig) -> Result<Self, EmbedError> {
        if config.endpoint.trim().is_empty() {
            return Err(EmbedError::Transport("embedding endpoint is empty".into()));
        }
        if config.model.trim().is_empty() {
            return Err(EmbedError::Transport("embedding model is empty".into()));
        }
        let client = reqwest::Client::builder()
            // Security constraint: **do not follow redirects** -- credentials should only be sent to the endpoint in the config.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(EMBED_CONNECT_TIMEOUT)
            .timeout(EMBED_TIMEOUT)
            .user_agent(concat!("shirley/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| EmbedError::Transport(error.to_string()))?;
        Ok(Self { client, config })
    }

    /// Endpoint (for diagnostics).
    #[allow(dead_code)]
    pub fn endpoint(&self) -> &str {
        &self.config.endpoint
    }

    /// Model identifier (written into the vector sidecar; changing the model must invalidate old vectors).
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Batch embed. Return order matches `texts` (backfilled by sorting on the response `index`).
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(MAX_BATCH) {
            let request = EmbedRequest {
                model: &self.config.model,
                input: chunk,
                encoding_format: "float",
            };
            let mut builder = self.client.post(&self.config.endpoint).json(&request);
            if let Some(key) = self.config.api_key.as_deref().filter(|k| !k.is_empty()) {
                builder = builder.bearer_auth(key);
            }
            let response = builder
                .send()
                .await
                .map_err(|error| EmbedError::Transport(error.to_string()))?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(EmbedError::Status {
                    status: status.as_u16(),
                    body: truncate(&body, MAX_ERROR_BODY),
                });
            }
            let mut parsed: EmbedResponse = response
                .json()
                .await
                .map_err(|error| EmbedError::Decode(error.to_string()))?;
            if parsed.data.len() != chunk.len() {
                return Err(EmbedError::Decode(format!(
                    "expected {} embeddings, got {}",
                    chunk.len(),
                    parsed.data.len()
                )));
            }
            // Some implementations return out of order; put them back by `index`.
            parsed.data.sort_by_key(|item| item.index);
            for item in parsed.data {
                if item.embedding.is_empty() {
                    return Err(EmbedError::Decode("empty embedding vector".into()));
                }
                out.push(item.embedding);
            }
        }
        Ok(out)
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
    /// Explicitly request a float array; most implementations accept it, and those that do not will ignore the key.
    encoding_format: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedItem>,
}

#[derive(Deserialize)]
struct EmbedItem {
    #[serde(default)]
    index: usize,
    embedding: Vec<f32>,
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_endpoint_or_model() {
        let bad_endpoint = Embedder::new(EmbeddingConfig {
            endpoint: "  ".into(),
            api_key: None,
            model: "m".into(),
        });
        assert!(bad_endpoint.is_err());

        let bad_model = Embedder::new(EmbeddingConfig {
            endpoint: "http://x/v1/embeddings".into(),
            api_key: None,
            model: "".into(),
        });
        assert!(bad_model.is_err());
    }

    #[test]
    fn builds_client_for_valid_config() {
        let embedder = Embedder::new(EmbeddingConfig {
            endpoint: "http://127.0.0.1:11434/v1/embeddings".into(),
            api_key: Some("k".into()),
            model: "bge-m3".into(),
        })
        .unwrap();
        assert_eq!(embedder.model(), "bge-m3");
        assert_eq!(embedder.endpoint(), "http://127.0.0.1:11434/v1/embeddings");
    }

    /// End-to-end smoke test connecting to a local Ollama. `#[ignore]` by default -- not run in CI / without Ollama.
    /// Manual verification: `cargo test -p Shirley --bin Shirley -- --ignored live_ollama`.
    /// The endpoint is Ollama's OpenAI-compatible entry point (`nomic-embed-text`, 768 dims).
    #[test]
    #[ignore = "requires a live Ollama at localhost:11434"]
    fn live_ollama_embeds() {
        let embedder = Embedder::new(EmbeddingConfig {
            endpoint: "http://localhost:11434/v1/embeddings".into(),
            api_key: None,
            model: "nomic-embed-text".into(),
        })
        .unwrap();
        // reqwest needs a Tokio reactor; drive this real HTTP with a one-off runtime.
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let out = runtime
            .block_on(embedder.embed(&[
                "Hello, my name is Sean".to_string(),
                "Shirley is a Rust coding agent".to_string(),
            ]))
            .expect("live ollama embedding should succeed");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 768, "nomic-embed-text outputs 768 dims");
        assert_eq!(out[1].len(), 768);
        // Two different texts should not get the same vector.
        assert_ne!(out[0], out[1]);
    }

    #[test]
    fn empty_input_short_circuits() {
        let embedder = Embedder::new(EmbeddingConfig {
            endpoint: "http://x/v1/embeddings".into(),
            api_key: None,
            model: "m".into(),
        })
        .unwrap();
        let out = futures::executor::block_on(embedder.embed(&[])).unwrap();
        assert!(out.is_empty());
    }
}
