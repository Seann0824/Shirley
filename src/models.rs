//! 模型目录（应用层）。
//!
//! 把"当前有哪些模型可选"这件事收敛到接口 [`ModelCatalog`] 后面：
//! 现在是写死的静态列表（[`StaticCatalog`]），将来若要按 provider 请求远端
//! 接口获取模型列表，只需换一个实现，`/model` 指令与选择器 UI 完全不用改。
//!
//! 接口刻意定成**异步**（返回 [`ModelListFuture`]）：静态列表用不上异步，但
//! 远端请求一定需要。现在就把签名固定下来，将来换实现才不用回头改调用方——
//! 这正是"对修改关闭、对扩展开放"的落点。

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// 一个可选模型。
///
/// `label` 给人看，`value` 给请求用，`provider` 标明来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    /// 展示名（UI 里给人看）。
    pub label: String,
    /// 请求时传给供应商的模型标识（落到 `ModelConfig::model`）。
    pub value: String,
    /// 来源供应商标识。现在只有一个；将来用于按 provider 分组 / 拉取。
    pub provider: String,
}

impl ModelEntry {
    pub fn new(
        label: impl Into<String>,
        value: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            provider: provider.into(),
        }
    }
}

/// 列出可选模型的异步结果。与 SDK 的 `ToolFuture` 同款：手写 boxed future，
/// 保证 trait 可以以 `dyn` 形式使用。
pub type ModelListFuture<'a> =
    Pin<Box<dyn Future<Output = Vec<ModelEntry>> + Send + 'a>>;

/// 模型目录接口——对外唯一稳定的契约。
///
/// 调用方只依赖这一件事：能拿到一份 [`ModelEntry`] 列表。列表从哪来
/// （写死 / 远端接口 / 缓存）是实现细节。
pub trait ModelCatalog: Send + Sync {
    fn list(&self) -> ModelListFuture<'_>;
}

/// 写死的静态目录。用于默认场景与测试。
#[derive(Debug, Default, Clone)]
pub struct StaticCatalog {
    entries: Vec<ModelEntry>,
}

impl StaticCatalog {
    pub fn new(entries: Vec<ModelEntry>) -> Self {
        Self { entries }
    }

    /// 内置默认列表。
    ///
    /// **这是占位实现**：真正接入远端后，模型清单应由 provider 接口返回，
    /// 这里只保留一份最小可用的手写清单，供 `/model` 立刻可用。
    /// 同步取出全部条目（静态数据本就在内存里，无需异步）。
    /// 供需要兜底列表的调用方使用，例如 `RemoteCatalog` 的 fallback。
    pub fn entries(&self) -> Vec<ModelEntry> {
        self.entries.clone()
    }

    pub fn builtin() -> Self {
        Self::new(vec![
            ModelEntry::new("deepseek-v4.1-flash", "deepseek-v4.1-flash", "local"),
            ModelEntry::new("hy3", "hy3", "local"),
        ])
    }
}

impl ModelCatalog for StaticCatalog {
    fn list(&self) -> ModelListFuture<'_> {
        let entries = self.entries.clone();
        Box::pin(async move { entries })
    }
}

/// 成功结果在内存里的有效期。
///
/// 模型清单在一次会话里几乎不会变，而每次打开 `/model` 都打一次网络会让选择器
/// 一直转圈（尤其是远端端点）。缓存一份，重复打开就是瞬时。
const CACHE_TTL: Duration = Duration::from_secs(60);

/// 单次拉取的超时。
///
/// `reqwest::Client` 默认**没有超时**：端点无响应时请求会一直挂着，
/// 选择器就一直显示"加载中"。给一个上界，超时后回退兜底列表。
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// 远端模型目录：请求 OpenAI 兼容的 `GET /v1/models` 接口。
///
/// 响应的 `data[].id` 作为请求用的模型值，`name`（缺省回退 `id`）作为展示名，
/// `owned_by` 作为 provider。接口拉取失败时**回退到给定的兜底列表**——列表拿不到
/// 不应该让 `/model` 整个不可用（远端抖动时仍能用上次的静态清单）。
///
/// 两处性能设计（此前每次都重新建 client + 无缓存，导致每次打开都要等）：
/// - **复用同一个 [`reqwest::Client`]**：它内部带连接池，复用才能省掉每次请求的
///   TCP + TLS 握手；每次 `Client::new()` 等于每次重新握手。
/// - **成功结果带 TTL 缓存**：只缓存成功，失败不缓存——端点抖一下不会把错误
///   钉死整个会话，下次打开会重试。
pub struct RemoteCatalog {
    /// 模型列表接口的完整地址，例如 `http://127.0.0.1:8788/v1/models`。
    endpoint: String,
    /// 可选的 Bearer 令牌。
    api_key: Option<String>,
    /// 拉取失败时的兜底条目。
    fallback: Vec<ModelEntry>,
    /// 复用的 HTTP client（连接池 + 超时）。
    client: reqwest::Client,
    /// 上次**成功**拉取的结果与时间戳。`None` = 还没有成功过。
    cache: Mutex<Option<(Instant, Vec<ModelEntry>)>>,
}

impl RemoteCatalog {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: Option<String>,
        fallback: Vec<ModelEntry>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            // 构造失败（极少见，例如 TLS 后端初始化不了）时退回默认 client：
            // 列表拿不到只是降级，不该让应用起不来。
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            endpoint: endpoint.into(),
            api_key,
            fallback,
            client,
            cache: Mutex::new(None),
        }
    }

    /// 命中未过期的缓存就返回副本。锁内不做任何 `.await`。
    fn cached(&self) -> Option<Vec<ModelEntry>> {
        let guard = self.cache.lock().ok()?;
        let (at, entries) = guard.as_ref()?;
        (at.elapsed() < CACHE_TTL).then(|| entries.clone())
    }

    /// 记录一次成功拉取。
    fn store(&self, entries: &[ModelEntry]) {
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some((Instant::now(), entries.to_vec()));
        }
    }
}

/// `/v1/models` 响应的反序列化结构。只取需要的字段，其余忽略。
#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelDto>,
}

#[derive(Deserialize)]
struct ModelDto {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    owned_by: Option<String>,
}

impl ModelCatalog for RemoteCatalog {
    fn list(&self) -> ModelListFuture<'_> {
        Box::pin(async move {
            // 命中未过期缓存：直接返回，不发任何网络请求。
            if let Some(entries) = self.cached() {
                return entries;
            }

            let mut request = self.client.get(&self.endpoint);
            if let Some(key) = &self.api_key {
                request = request.bearer_auth(key);
            }
            let parsed = async {
                let response = request.send().await.ok()?;
                if !response.status().is_success() {
                    return None;
                }
                let body: ModelsResponse = response.json().await.ok()?;
                Some(
                    body.data
                        .into_iter()
                        .map(|m| {
                            let label = m.name.unwrap_or_else(|| m.id.clone());
                            let provider = m.owned_by.unwrap_or_else(|| "remote".to_owned());
                            ModelEntry::new(label, m.id, provider)
                        })
                        .collect::<Vec<_>>(),
                )
            }
            .await;

            match parsed {
                Some(entries) if !entries.is_empty() => {
                    self.store(&entries);
                    entries
                }
                // 请求失败或返回空：回退到兜底列表，保证 `/model` 仍可用。
                // 刻意**不写缓存**：一次抖动不该被钉死一整个 TTL，下次打开会重试。
                _ => self.fallback.clone(),
            }
        })
    }
}

/// 三种协议的 `base_url` 收尾。它们都指向同一个模型列表接口。
///
/// `base_url` 是**完整请求端点**（见 SDK 的 `ModelConfig`），各协议收尾不同：
/// `chat_completions` 收在 `/chat/completions`，`anthropic_messages` 收在
/// `/messages`，`responses` 收在 `/responses`。只认其中一种，另两种就会拼出
/// 不存在的地址（例如 `…/v1/messages/models`），列表拉取失败后静默回退到
/// 内置静态清单——表现为 `/model` 选择器里看不到真实模型。
const PROTOCOL_ENDPOINT_SUFFIXES: [&str; 3] = [
    "/chat/completions",
    "/messages",
    "/responses",
];

/// 从 `base_url` 推导模型列表接口地址。
///
/// 命中已知协议后缀就把它换成 `/models`：
///
/// | 协议 | `base_url` | 推导结果 |
/// | --- | --- | --- |
/// | ChatCompletions | `https://host/v1/chat/completions` | `https://host/v1/models` |
/// | AnthropicMessages | `https://host/v1/messages` | `https://host/v1/models` |
/// | Responses | `https://host/responses` | `https://host/models` |
///
/// 都不命中时（例如 `base_url` 本身就是 `…/v1`）退回在末尾拼 `/models`。
/// 若服务端的列表地址不遵循这个约定，用配置里的 `models_url` 显式覆盖。
pub fn models_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    for suffix in PROTOCOL_ENDPOINT_SUFFIXES {
        if let Some(prefix) = trimmed.strip_suffix(suffix) {
            return format!("{prefix}/models");
        }
    }
    format!("{trimmed}/models")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn static_catalog_lists_builtin_entries() {
        let catalog = StaticCatalog::builtin();
        let entries = block_on(catalog.list());
        assert!(!entries.is_empty(), "内置列表不应为空");
        // 当前默认模型必须在列表里，否则选择器无法高亮。
        assert!(
            entries.iter().any(|e| e.value == "deepseek-v4.1-flash"),
            "应包含当前默认模型: {entries:?}"
        );
    }

    #[test]
    fn derives_models_endpoint_from_base_url() {
        // ChatCompletions：剥掉 /chat/completions
        assert_eq!(
            models_endpoint("http://127.0.0.1:8788/v1/chat/completions"),
            "http://127.0.0.1:8788/v1/models"
        );
        // 不含协议后缀：退回末尾拼 /models
        assert_eq!(
            models_endpoint("http://127.0.0.1:8788/v1"),
            "http://127.0.0.1:8788/v1/models"
        );
    }

    // 需要本地 `/v1/models` 服务（127.0.0.1:8788）。默认忽略，手动跑：
    //   cargo test --bin Shirley -- --ignored remote_catalog_fetches_live
    // 远端不可达时应回退到兜底列表，保证 `/model` 仍可用。
    #[tokio::test]
    async fn remote_catalog_falls_back_when_unreachable() {
        let fallback = vec![ModelEntry::new("fb", "fb", "local")];
        let catalog = RemoteCatalog::new(
            "http://127.0.0.1:1/v1/models",
            None,
            fallback.clone(),
        );
        assert_eq!(catalog.list().await, fallback);
    }

    /// 回归：成功拉取会命中 TTL 缓存，第二次 `list()` 不再发网络请求。
    ///
    /// 此前每次 `list()` 都新建 `reqwest::Client` 并重新请求，导致每次打开
    /// `/model` 都要等一次完整往返。这里用最小 HTTP 桩 + 请求计数把它钉住。
    #[tokio::test]
    async fn remote_catalog_caches_successful_fetch() {
        use std::io::{Read, Write};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_server = Arc::clone(&hits);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                // GET 无 body：读到请求头结束就够了。
                let mut buf = Vec::new();
                let mut chunk = [0u8; 512];
                while let Ok(n) = stream.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n".as_slice()) {
                        break;
                    }
                }
                hits_server.fetch_add(1, Ordering::SeqCst);
                let body = r#"{"data":[{"id":"m1"},{"id":"m2"}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        let catalog = RemoteCatalog::new(
            format!("http://{addr}/v1/models"),
            None,
            vec![ModelEntry::new("fb", "fb", "local")],
        );

        let first = catalog.list().await;
        let second = catalog.list().await;

        assert_eq!(
            first.iter().map(|e| e.value.as_str()).collect::<Vec<_>>(),
            vec!["m1", "m2"],
            "首次应拉到远端列表"
        );
        assert_eq!(second, first, "第二次应命中缓存，返回同一份列表");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "命中缓存时不应再发网络请求"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn remote_catalog_fetches_live() {
        let catalog = RemoteCatalog::new(
            models_endpoint("http://127.0.0.1:8788/v1/chat/completions"),
            None,
            vec![ModelEntry::new("fallback", "fallback", "x")],
        );
        let entries = catalog.list().await;
        assert!(!entries.is_empty());
        // 不应回退到 fallback（说明真的拉到了远端列表）。
        assert!(
            !entries.iter().all(|e| e.value == "fallback"),
            "应拉到远端列表而非兜底: {entries:?}"
        );
        eprintln!("拉取到 {} 个模型，例如 {:?}", entries.len(), entries.first());
    }

    /// 活体诊断：按**工作区真实配置**构造 `RemoteCatalog` 并拉取，
    /// 验证 `models_endpoint` 对当前协议后缀的推导是否正确。
    ///
    /// 依赖外部服务与工作区配置，默认忽略，手动跑：
    ///   cargo test --bin Shirley live_remote_catalog_from_workspace_config -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_remote_catalog_from_workspace_config() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let settings = crate::settings::Settings::load_default(&root).expect("装载配置失败");
        let url = settings
            .models_url
            .clone()
            .unwrap_or_else(|| models_endpoint(&settings.base_url));
        eprintln!("[catalog] base_url   = {}", settings.base_url);
        eprintln!("[catalog] models_url = {url}");

        let fallback = StaticCatalog::builtin().entries();
        let catalog = RemoteCatalog::new(url, settings.api_key.clone(), fallback.clone());
        let entries = catalog.list().await;
        eprintln!(
            "[catalog] 拉到 {} 个模型，前 3 个: {:?}",
            entries.len(),
            entries.iter().take(3).map(|e| &e.value).collect::<Vec<_>>()
        );
        assert_ne!(entries, fallback, "不应回退到内置兜底列表（说明真的拉到了远端）");
    }

    #[test]
    fn custom_entries_round_trip() {
        let catalog = StaticCatalog::new(vec![ModelEntry::new("L", "v", "p")]);
        let entries = block_on(catalog.list());
        assert_eq!(entries, vec![ModelEntry::new("L", "v", "p")]);
    }
}
