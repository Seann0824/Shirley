//! 语义向量：OpenAI 兼容 `POST /v1/embeddings` 客户端（`docs/memory.md` §10 / V2.5）。
//!
//! **诚实降级**：本模块只负责"怎么调一个 OpenAI 兼容的 embeddings 接口"。是否启用
//! 由配置决定——没配 `[memory] embedding_*` 时 [`crate::bootstrap`] 不会构造
//! [`Embedder`]，检索退化为纯 BM25（中文已走 bigram，可用）。**绝不假装有语义腿。**
//!
//! 安全约束对齐 `tools/web_search.rs`：不跟随重定向、连接 / 总超时、响应体封顶。
//! 不引向量库 / 新 crate——向量落 sidecar 文件（见 [`super::vector`]）。

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 单次请求总超时。
pub const EMBED_TIMEOUT: Duration = Duration::from_secs(15);
/// 连接超时。
pub const EMBED_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// 单次请求最多嵌入的文本数（分批，防呆）。
pub const MAX_BATCH: usize = 64;
/// 错误体截断长度。
const MAX_ERROR_BODY: usize = 300;

/// embeddings 调用错误。
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// 网络 / 构建客户端失败。
    #[error("embedding transport error: {0}")]
    Transport(String),
    /// 非 2xx 响应。
    #[error("embedding endpoint returned {status}: {body}")]
    Status { status: u16, body: String },
    /// 响应不是预期的 JSON 结构（含 base64 向量——本实现只认 float 数组）。
    #[error("embedding response malformed: {0}")]
    Decode(String),
}

/// embedding 端点配置。
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// 完整地址，例如 `http://127.0.0.1:11434/v1/embeddings`。
    pub endpoint: String,
    /// 密钥；`None` / 空串按"无鉴权"处理（本地服务常见）。
    pub api_key: Option<String>,
    /// 模型标识，例如 `text-embedding-3-small` / `bge-m3`。
    pub model: String,
}

/// OpenAI 兼容 embeddings 客户端（可 `Clone`，内部 `reqwest::Client` 共享连接池）。
#[derive(Clone)]
pub struct Embedder {
    client: reqwest::Client,
    config: EmbeddingConfig,
}

impl Embedder {
    /// 构造客户端（校验端点非空）。
    pub fn new(config: EmbeddingConfig) -> Result<Self, EmbedError> {
        if config.endpoint.trim().is_empty() {
            return Err(EmbedError::Transport("embedding endpoint is empty".into()));
        }
        if config.model.trim().is_empty() {
            return Err(EmbedError::Transport("embedding model is empty".into()));
        }
        let client = reqwest::Client::builder()
            // 安全约束：**不跟随重定向**——凭据只应发往配置里那个端点。
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(EMBED_CONNECT_TIMEOUT)
            .timeout(EMBED_TIMEOUT)
            .user_agent(concat!("shirley/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| EmbedError::Transport(error.to_string()))?;
        Ok(Self { client, config })
    }

    /// 端点（诊断用）。
    #[allow(dead_code)]
    pub fn endpoint(&self) -> &str {
        &self.config.endpoint
    }

    /// 模型标识（写进向量 sidecar，模型换了要让旧向量失效）。
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// 批量嵌入。返回顺序与 `texts` 一致（按响应里的 `index` 排序回填）。
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
            // 有些实现乱序返回，按 `index` 归位。
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
    /// 显式要求 float 数组；多数实现接受，不接受的会忽略该键。
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

    /// 实连本地 Ollama 的端到端冒烟测试。默认 `#[ignore]`——CI / 无 Ollama 环境不跑。
    /// 手动验证：`cargo test -p Shirley --bin Shirley -- --ignored live_ollama`。
    /// 端点为 Ollama 的 OpenAI 兼容入口（`nomic-embed-text`，768 维）。
    #[test]
    #[ignore = "requires a live Ollama at localhost:11434"]
    fn live_ollama_embeds() {
        let embedder = Embedder::new(EmbeddingConfig {
            endpoint: "http://localhost:11434/v1/embeddings".into(),
            api_key: None,
            model: "nomic-embed-text".into(),
        })
        .unwrap();
        // reqwest 需要 Tokio reactor；用一次性 runtime 驱动这次真实 HTTP。
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let out = runtime
            .block_on(embedder.embed(&[
                "你好，我叫 Sean".to_string(),
                "Shirley 是一个 Rust coding agent".to_string(),
            ]))
            .expect("live ollama embedding should succeed");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 768, "nomic-embed-text 输出 768 维");
        assert_eq!(out[1].len(), 768);
        // 两个不同文本不应得到相同向量。
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
