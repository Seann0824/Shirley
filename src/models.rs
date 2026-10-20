//! Model catalog (application layer).
//!
//! Converges "which models are currently selectable" behind the [`ModelCatalog`] interface:
//! it is a hardcoded static list for now ([`StaticCatalog`]); in the future, if we want to
//! fetch the model list from a provider's remote endpoint, we only swap in another implementation and
//! the `/model` command and selector UI stay untouched.
//! The interface is deliberately **async** (returns [`ModelListFuture`]): a static list does not need async, but
//! a remote request certainly does. Fixing the signature now means we will not have to revisit the caller when we
//! swap the implementation -- exactly the point of "closed to modification, open to extension".

use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// A selectable model.
///
/// `label` is for humans, `value` is for the request, `provider` marks the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    /// Full connection for a host-local configured provider; never serialized to the UI.
    pub configured: Option<crate::model_providers::ConfiguredModel>,
    /// Display name (shown to humans in the UI).
    pub label: String,
    /// The model identifier passed to the provider at request time (lands in `ModelConfig::model`).
    pub value: String,
    /// Source provider identifier. Only one for now; in the future used for grouping / fetching by provider.
    pub provider: String,
    /// The **tool transport mode** declared by the provider (`docs/tool-calling.md` V supplement).
    ///
    /// Each item in `GET /v1/models` may carry a `tool_mode` field (`"native" | "content"`, optional).
    /// The application layer uses it to pick the `ToolCalling` for the model: `content` means the model does not support
    /// native function calling and should go through the text protocol. `None` = not declared (falls back to the default `Native`).
    pub tool_mode: Option<String>,
    /// Whether the model accepts image input, declared by the provider (`supports_images`, optional).
    ///
    /// Same shape as `tool_mode`: a **capability fact carried by catalog data**, applied per model when
    /// switching. `Some(true/false)` = declared (overrides the global `[provider] supports_images`);
    /// `None` = not declared (falls back to the global setting). See `bootstrap::model_config_for`.
    pub supports_images: Option<bool>,
}

impl ModelEntry {
    pub fn new(
        label: impl Into<String>,
        value: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        Self {
            configured: None,
            label: label.into(),
            value: value.into(),
            provider: provider.into(),
            tool_mode: None,
            supports_images: None,
        }
    }

    /// Chain-set the tool transport mode (used when building `StaticCatalog` fallback entries).
    pub fn with_tool_mode(mut self, tool_mode: Option<String>) -> Self {
        self.tool_mode = tool_mode;
        self
    }

    /// Chain-set the image capability declared by the catalog (used when building entries).
    pub fn with_supports_images(mut self, supports_images: Option<bool>) -> Self {
        self.supports_images = supports_images;
        self
    }
}

/// The async result of listing selectable models. Same style as the SDK's `ToolFuture`: a hand-written boxed future,
/// so the trait can be used as `dyn`.
pub type ModelListFuture<'a> = Pin<Box<dyn Future<Output = Vec<ModelEntry>> + Send + 'a>>;

/// Model catalog interface -- the only stable contract exposed outward.
///
/// The caller depends on exactly one thing: being able to get a list of [`ModelEntry`]. Where the list comes from
/// (hardcoded / remote endpoint / cache) is an implementation detail.
pub trait ModelCatalog: Send + Sync {
    fn list(&self) -> ModelListFuture<'_>;
}

/// Hardcoded static catalog. Used for the default scenario and tests.
#[derive(Debug, Default, Clone)]
pub struct StaticCatalog {
    entries: Vec<ModelEntry>,
}

impl StaticCatalog {
    pub fn new(entries: Vec<ModelEntry>) -> Self {
        Self { entries }
    }

    /// Built-in default list.
    ///
    /// **This is a placeholder implementation**: once the remote is wired up, the model list should be returned by the provider
    /// endpoint; here we keep only a minimal working hand-written list so `/model` is usable right away.
    /// Synchronously returns all entries (static data is in memory anyway, no async needed).
    /// For callers that need a fallback list, e.g. `RemoteCatalog`'s fallback.
    pub fn entries(&self) -> Vec<ModelEntry> {
        self.entries.clone()
    }

    pub fn builtin() -> Self {
        Self::new(vec![
            // Known multimodal model: declare image input so it works out of the box.
            ModelEntry::new("deepseek-v4.1-flash", "deepseek-v4.1-flash", "local")
                .with_supports_images(Some(true)),
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

/// **Normalize a model entry's tool transport mode into catalog data**.
///
/// Priority: the catalog-declared `tool_mode` (trimmed, lowercased) > provider inference > `None`.
/// Figma Make's model catalog does not yet declare tool capability; fill it in as `content` based on its provider / model-name
/// prefix -- **this is data normalization, not a parsing branch** (`docs/tool-calling-refactor.md`
/// Step 3: capability declarations belong to catalog data; parsing logic does not know about providers).
pub(crate) fn normalize_tool_mode(
    id: &str,
    provider: &str,
    declared: Option<String>,
) -> Option<String> {
    if let Some(mode) = declared
        .map(|mode| mode.trim().to_ascii_lowercase())
        .filter(|mode| !mode.is_empty())
    {
        return Some(mode);
    }
    if provider.eq_ignore_ascii_case("figma-make") || id.starts_with("figma-make-") {
        return Some("content".to_owned());
    }
    None
}

/// **Normalize a model's image capability into catalog data** (`docs/multimodal.md`).
///
/// Same shape as [`normalize_tool_mode`]: it turns the many spellings providers use into one
/// `bool`. Priority (`Continue.dev`-style, but with an **optimistic** default):
///
/// 1. explicit declaration (`supports_images`)
/// 2. `supports_vision` (LiteLLM / models.dev legacy)
/// 3. `architecture.input_modalities` (OpenRouter) / `modalities.input` (models.dev) — an
///    `image` entry means yes, a non-empty list without `image` means text-only
/// 4. built-in model-name table (known vision families)
/// 5. **default `true`** — optimistic: try sending once, and if the provider really rejects the
///    image the adapter strips it and resends silently (`docs/multimodal.md`). Never a wall.
///
/// The user is never asked and never sees a capability toggle: a wrong guess costs at most one
/// extra failed request, then degrades invisibly.
pub(crate) fn normalize_supports_images(
    id: &str,
    declared: Option<bool>,
    supports_vision: Option<bool>,
    input_modalities: Option<&[String]>,
    modalities_input: Option<&[String]>,
) -> bool {
    if let Some(value) = declared {
        return value;
    }
    if let Some(value) = supports_vision {
        return value;
    }
    for list in [input_modalities, modalities_input].into_iter().flatten() {
        if !list.is_empty() {
            return list.iter().any(|m| m.eq_ignore_ascii_case("image"));
        }
    }
    if name_suggests_vision(id) {
        return true;
    }
    // Optimistic default: attempt with images, degrade silently on real rejection.
    true
}

/// Convenience over a raw `/v1/models` item (used by the provider-registry path, which keeps the
/// item as `serde_json::Value`).
pub(crate) fn normalize_supports_images_from_value(id: &str, value: &serde_json::Value) -> bool {
    fn string_array(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
        value.and_then(|v| v.as_array()).map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
    }
    let declared = value.get("supports_images").and_then(|v| v.as_bool());
    let supports_vision = value.get("supports_vision").and_then(|v| v.as_bool());
    let input_modalities = string_array(
        value
            .get("architecture")
            .and_then(|a| a.get("input_modalities")),
    );
    let modalities_input = string_array(value.get("modalities").and_then(|m| m.get("input")));
    normalize_supports_images(
        id,
        declared,
        supports_vision,
        input_modalities.as_deref(),
        modalities_input.as_deref(),
    )
}

/// Built-in name table: model families that are known to accept images. Only used as a hint;
/// the final fallback is optimistic `true`.
fn name_suggests_vision(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    const VISION_MARKERS: [&str; 16] = [
        "vision",
        "multimodal",
        "-vl",
        "vl-",
        "gpt-4o",
        "gpt-4.1",
        "gpt-5",
        "o4",
        "claude-3",
        "claude-4",
        "claude-sonnet",
        "gemini",
        "llava",
        "pixtral",
        "glm-4v",
        "internvl",
    ];
    VISION_MARKERS.iter().any(|marker| id.contains(marker))
}

/// The in-memory lifetime of a successful result.
///
/// The model list almost never changes within a session, and hitting the network every time `/model` is opened makes the selector
/// spin forever (especially remote endpoints). Cache one copy; reopening is instant.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// The timeout for a single fetch.
///
/// `reqwest::Client` has **no timeout** by default: when the endpoint is unresponsive the request hangs forever,
/// and the selector keeps showing "loading". Give an upper bound; on timeout fall back to the fallback list.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Remote model catalog: requests the OpenAI-compatible `GET /v1/models` endpoint.
///
/// The response's `data[].id` is the model value for requests, `name` (falling back to `id`) is the display name,
/// and `owned_by` is the provider. When the fetch fails it **falls back to the given fallback list** -- not getting the list
/// should not make `/model` entirely unusable (during a remote hiccup the last static list still works).
///
/// Two performance designs (previously each call rebuilt the client and had no cache, so every open waited):
/// - **Reuse one [`reqwest::Client`]**: it carries a connection pool internally, and reuse is what saves the
///   TCP + TLS handshake per request; `Client::new()` each time means re-handshaking each time.
/// - **Successful results are TTL-cached**: only successes are cached, failures are not -- an endpoint hiccup will not
///   pin the error for the whole session; the next open retries.
pub struct RemoteCatalog {
    /// The full address of the model list endpoint, e.g. `http://127.0.0.1:8788/v1/models`.
    endpoint: String,
    /// Optional Bearer token.
    api_key: Option<String>,
    /// Fallback entries when the fetch fails.
    fallback: Vec<ModelEntry>,
    /// Reused HTTP client (connection pool + timeout).
    client: reqwest::Client,
    /// The last **successful** fetch result and timestamp. `None` = never succeeded yet.
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
            // On construction failure (rare, e.g. the TLS backend cannot initialize) fall back to the default client:
            // not getting the list is just a degradation; it should not stop the app from starting.
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            endpoint: endpoint.into(),
            api_key,
            fallback,
            client,
            cache: Mutex::new(None),
        }
    }

    /// Return a copy if an unexpired cache is hit. No `.await` under the lock.
    fn cached(&self) -> Option<Vec<ModelEntry>> {
        let guard = self.cache.lock().ok()?;
        let (at, entries) = guard.as_ref()?;
        (at.elapsed() < CACHE_TTL).then(|| entries.clone())
    }

    /// Record a successful fetch.
    fn store(&self, entries: &[ModelEntry]) {
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some((Instant::now(), entries.to_vec()));
        }
    }
}

/// Deserialization structure for the `/v1/models` response. Only the needed fields are taken, the rest ignored.
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
    /// The tool transport mode declared by the provider (optional, see [`ModelEntry::tool_mode`]).
    #[serde(default)]
    tool_mode: Option<String>,
    /// Whether the model accepts image input (optional, see [`ModelEntry::supports_images`]).
    #[serde(default)]
    supports_images: Option<bool>,
    /// LiteLLM / models.dev legacy spelling of the same capability.
    #[serde(default)]
    supports_vision: Option<bool>,
    /// OpenRouter-style `architecture.input_modalities`.
    #[serde(default)]
    architecture: Option<ArchitectureDto>,
    /// models.dev-style `modalities.input`.
    #[serde(default)]
    modalities: Option<ModalitiesDto>,
}

#[derive(Deserialize)]
struct ArchitectureDto {
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ModalitiesDto {
    #[serde(default)]
    input: Option<Vec<String>>,
}

impl ModelCatalog for RemoteCatalog {
    fn list(&self) -> ModelListFuture<'_> {
        Box::pin(async move {
            // Unexpired cache hit: return directly, no network request.
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
                            let tool_mode = normalize_tool_mode(&m.id, &provider, m.tool_mode);
                            // Multi-format capability detection (see `normalize_supports_images`).
                            let input_modalities = m
                                .architecture
                                .as_ref()
                                .and_then(|a| a.input_modalities.as_deref());
                            let modalities_input =
                                m.modalities.as_ref().and_then(|mo| mo.input.as_deref());
                            let supports_images = normalize_supports_images(
                                &m.id,
                                m.supports_images,
                                m.supports_vision,
                                input_modalities,
                                modalities_input,
                            );
                            ModelEntry::new(label, m.id, provider)
                                .with_tool_mode(tool_mode)
                                .with_supports_images(Some(supports_images))
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
                // Request failed or returned empty: fall back to the fallback list, keeping `/model` usable.
                // Deliberately **do not cache**: one hiccup should not be pinned for a whole TTL; the next open retries.
                _ => self.fallback.clone(),
            }
        })
    }
}

/// Trailing suffix handling for the three protocols' `base_url`. They all point at the same model list endpoint.
///
/// `base_url` is the **full request endpoint** (see the SDK's `ModelConfig`); each protocol ends differently:
/// `chat_completions` ends at `/chat/completions`, `anthropic_messages` ends at
/// `/messages`, `responses` ends at `/responses`. Recognizing only one of them means the other two assemble a
/// nonexistent address (e.g. `.../v1/messages/models`), the list fetch fails and silently falls back to the
/// built-in static list -- showing up as no real models in the `/model` selector.
const PROTOCOL_ENDPOINT_SUFFIXES: [&str; 3] = ["/chat/completions", "/messages", "/responses"];

/// Derive the model list endpoint address from `base_url`.
///
/// When a known protocol suffix is hit, replace it with `/models`:
///
/// | Protocol | `base_url` | Derived result |
/// | --- | --- | --- |
/// | ChatCompletions | `https://host/v1/chat/completions` | `https://host/v1/models` |
/// | AnthropicMessages | `https://host/v1/messages` | `https://host/v1/models` |
/// | Responses | `https://host/responses` | `https://host/models` |
///
/// When none is hit (e.g. `base_url` is itself `.../v1`), fall back to appending `/models` at the end.
/// If the server's list address does not follow this convention, override it explicitly with `models_url` in config.
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
        assert!(!entries.is_empty(), "the built-in list should not be empty");
        // The current default model must be in the list, otherwise the selector cannot highlight it.
        assert!(
            entries.iter().any(|e| e.value == "deepseek-v4.1-flash"),
            "should contain the current default model: {entries:?}"
        );
    }

    #[test]
    fn derives_models_endpoint_from_base_url() {
        // ChatCompletions: strip /chat/completions
        assert_eq!(
            models_endpoint("http://127.0.0.1:8788/v1/chat/completions"),
            "http://127.0.0.1:8788/v1/models"
        );
        // No protocol suffix: fall back to appending /models
        assert_eq!(
            models_endpoint("http://127.0.0.1:8788/v1"),
            "http://127.0.0.1:8788/v1/models"
        );
    }

    // Requires a local `/v1/models` service (127.0.0.1:8788). Ignored by default; run manually:
    //   cargo test --bin Shirley -- --ignored remote_catalog_fetches_live
    // When the remote is unreachable it should fall back to the fallback list, keeping `/model` usable.
    #[tokio::test]
    async fn remote_catalog_falls_back_when_unreachable() {
        let fallback = vec![ModelEntry::new("fb", "fb", "local")];
        let catalog = RemoteCatalog::new("http://127.0.0.1:1/v1/models", None, fallback.clone());
        assert_eq!(catalog.list().await, fallback);
    }

    /// Regression: a successful fetch hits the TTL cache, so the second `list()` sends no network request.
    ///
    /// Previously every `list()` built a new `reqwest::Client` and re-requested, making every open of
    /// `/model` wait for a full round trip. A minimal HTTP stub + request counter pins this down.
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
                // GET with no body: reading until the end of the request headers is enough.
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
            "the first should fetch the remote list"
        );
        assert_eq!(
            second, first,
            "the second should hit the cache and return the same list"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "hitting the cache should not send another network request"
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
        // Should not fall back to fallback (which means the remote list was really fetched).
        assert!(
            !entries.iter().all(|e| e.value == "fallback"),
            "should fetch the remote list rather than the fallback: {entries:?}"
        );
        eprintln!(
            "fetched {} models, e.g. {:?}",
            entries.len(),
            entries.first()
        );
    }

    /// Live diagnostic: build a `RemoteCatalog` from the **real workspace config** and fetch, to
    /// verify that `models_endpoint` derives the current protocol suffix correctly.
    ///
    /// Depends on an external service and the workspace config; ignored by default, run manually:
    //   cargo test --bin Shirley live_remote_catalog_from_workspace_config -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_remote_catalog_from_workspace_config() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let settings =
            crate::settings::Settings::load_default(&root).expect("failed to load settings");
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
            "[catalog] fetched {} models, first 3: {:?}",
            entries.len(),
            entries.iter().take(3).map(|e| &e.value).collect::<Vec<_>>()
        );
        assert_ne!(
            entries, fallback,
            "should not fall back to the built-in list (which means the remote was really fetched)"
        );
    }

    #[test]
    fn custom_entries_round_trip() {
        let catalog = StaticCatalog::new(vec![ModelEntry::new("L", "v", "p")]);
        let entries = block_on(catalog.list());
        assert_eq!(entries, vec![ModelEntry::new("L", "v", "p")]);
    }
}
