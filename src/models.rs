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

/// 远端模型目录：请求 OpenAI 兼容的 `GET /v1/models` 接口。
///
/// 响应的 `data[].id` 作为请求用的模型值，`name`（缺省回退 `id`）作为展示名，
/// `owned_by` 作为 provider。接口拉取失败时**回退到给定的兜底列表**——列表拿不到
/// 不应该让 `/model` 整个不可用（远端抖动时仍能用上次的静态清单）。
pub struct RemoteCatalog {
    /// 模型列表接口的完整地址，例如 `http://127.0.0.1:8788/v1/models`。
    endpoint: String,
    /// 可选的 Bearer 令牌。
    api_key: Option<String>,
    /// 拉取失败时的兜底条目。
    fallback: Vec<ModelEntry>,
}

impl RemoteCatalog {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: Option<String>,
        fallback: Vec<ModelEntry>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key,
            fallback,
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
            let client = reqwest::Client::new();
            let mut request = client.get(&self.endpoint);
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
                Some(entries) if !entries.is_empty() => entries,
                // 请求失败或返回空：回退到兜底列表，保证 `/model` 仍可用。
                _ => self.fallback.clone(),
            }
        })
    }
}

/// 从 chat completions 的 `base_url` 推导模型列表接口地址。
///
/// 约定 base_url 以 `/chat/completions` 结尾（见 `.env` 的 `LOCAL_BASE_URL`），
/// 把它替换成 `/models` 即得列表接口；否则退回在末尾拼 `/models`。
pub fn models_endpoint(base_url: &str) -> String {
    if let Some(prefix) = base_url.strip_suffix("/chat/completions") {
        return format!("{prefix}/models");
    }
    format!("{}/models", base_url.trim_end_matches('/'))
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
        assert_eq!(
            models_endpoint("http://127.0.0.1:8788/v1/chat/completions"),
            "http://127.0.0.1:8788/v1/models"
        );
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

    #[test]
    fn custom_entries_round_trip() {
        let catalog = StaticCatalog::new(vec![ModelEntry::new("L", "v", "p")]);
        let entries = block_on(catalog.list());
        assert_eq!(entries, vec![ModelEntry::new("L", "v", "p")]);
    }
}
