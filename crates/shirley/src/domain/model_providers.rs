//! Host-local provider profiles. Selection keys identify provider + model; credentials never enter wire DTOs.
use crate::models::{ModelCatalog, ModelEntry, ModelListFuture};
use serde::{Deserialize, Serialize};
use shirley_agent_sdk::{ModelConfig, ModelProtocol, ToolCallingMode};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FILE: &str = "model-providers.json";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    ChatCompletions,
    Responses,
    AnthropicMessages,
}
impl Protocol {
    fn sdk(self) -> ModelProtocol {
        match self {
            Self::ChatCompletions => ModelProtocol::ChatCompletions,
            Self::Responses => ModelProtocol::Responses,
            Self::AnthropicMessages => ModelProtocol::AnthropicMessages,
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat/completions",
            Self::Responses => "responses",
            Self::AnthropicMessages => "messages",
        }
    }
}
fn default_context() -> u64 {
    65536
}
fn default_tool_mode() -> String {
    "native".into()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelDefinition {
    pub id: String,
    pub name: String,
    #[serde(default = "default_context")]
    pub context_window_tokens: u64,
    pub supports_images: bool,
    #[serde(default = "default_tool_mode")]
    pub tool_mode: String,
    pub thinking: bool,
    pub reasoning_effort: Option<String>,
}
impl Default for ModelDefinition {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            context_window_tokens: default_context(),
            // Optimistic default (`docs/multimodal.md`): assume image input works and degrade
            // silently on a real rejection instead of denying up front.
            supports_images: true,
            tool_mode: default_tool_mode(),
            thinking: false,
            reasoning_effort: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub models_url: Option<String>,
    pub api_key: Option<String>,
    pub default_model: String,
    pub models: Vec<ModelDefinition>,
    pub extra_body: Option<serde_json::Value>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Registry {
    pub providers: Vec<Provider>,
    pub default_selection: Option<String>,
}

#[derive(Serialize)]
pub struct ProviderView {
    #[serde(flatten)]
    pub provider: Provider,
    pub has_api_key: bool,
}
#[derive(Serialize)]
pub struct RegistryView {
    pub revision: String,
    pub providers: Vec<ProviderView>,
    pub default_selection: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveProvider {
    pub revision: String,
    pub provider: Provider,
    #[serde(default)]
    pub clear_api_key: bool,
    #[serde(default)]
    pub set_default: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteProvider {
    pub revision: String,
    pub id: String,
}

pub fn selection(provider: &str, model: &str) -> String {
    format!(
        "provider:{}:{}",
        provider,
        url::form_urlencoded::byte_serialize(model.as_bytes()).collect::<String>()
    )
}

pub fn load(global: &Path) -> Result<Registry, String> {
    match std::fs::read(global.join(FILE)) {
        Ok(bytes) => {
            let mut registry: Registry =
                serde_json::from_slice(&bytes).map_err(|e| format!("模型配置文件无效：{e}"))?;
            let mut ids = std::collections::BTreeSet::new();
            for provider in &mut registry.providers {
                validate(provider, true)?;
                if !ids.insert(provider.id.clone()) {
                    return Err("服务商 ID 重复".into());
                }
            }
            if registry.default_selection.as_ref().is_some_and(|key| {
                !registry
                    .providers
                    .iter()
                    .any(|p| p.models.iter().any(|m| selection(&p.id, &m.id) == *key))
            }) {
                return Err("默认模型已不存在，请修复模型配置文件".into());
            }
            Ok(registry)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(e) => Err(e.to_string()),
    }
}
fn revision(registry: &Registry) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        &serde_json::to_vec(registry).unwrap(),
    )
    .to_string()
}
fn view(registry: Registry) -> RegistryView {
    let revision = revision(&registry);
    RegistryView {
        revision,
        default_selection: registry.default_selection,
        providers: registry
            .providers
            .into_iter()
            .map(|mut provider| {
                let has_api_key = provider.api_key.take().is_some_and(|key| !key.is_empty());
                ProviderView {
                    provider,
                    has_api_key,
                }
            })
            .collect(),
    }
}
pub fn get(global: &Path) -> Result<RegistryView, String> {
    Ok(view(load(global)?))
}
fn check_revision(registry: &Registry, expected: &str) -> Result<(), String> {
    if revision(registry) != expected {
        return Err("模型配置已被其他窗口修改，请重新加载后再保存".into());
    }
    Ok(())
}
fn persist(global: &Path, registry: &Registry) -> Result<RegistryView, String> {
    crate::memory::store::atomic_write(
        &global.join(FILE),
        &serde_json::to_vec_pretty(registry).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(view(registry.clone()))
}

fn validate_url(text: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(text).map_err(|_e| "接口地址无效")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("接口地址必须是 HTTP/HTTPS 地址，且不能包含账号、密码或片段".into());
    }
    Ok(url)
}
/// Accept a service root, /v1 base, or full protocol endpoint. Custom full paths are preserved.
pub fn endpoint(provider: &Provider) -> Result<String, String> {
    let mut url = validate_url(provider.base_url.trim())?;
    let path = url.path().trim_end_matches('/');
    let suffix = provider.protocol.suffix();
    let path = if path.is_empty() {
        format!("/v1/{suffix}")
    } else if path.ends_with("/v1") {
        format!("{path}/{suffix}")
    } else {
        path.to_string()
    };
    url.set_path(&path);
    Ok(url.to_string())
}
fn validate(provider: &mut Provider, require_models: bool) -> Result<(), String> {
    provider.id = provider.id.trim().into();
    if provider.id.is_empty()
        || provider.id.len() > 100
        || !provider
            .id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("服务商 ID 无效".into());
    }
    provider.name = provider.name.trim().into();
    provider.default_model = provider.default_model.trim().into();
    if provider.name.is_empty() {
        return Err("请填写服务商名称".into());
    }
    provider.base_url = provider.base_url.trim().trim_end_matches('/').into();
    endpoint(provider)?;
    let path = validate_url(&provider.base_url)?
        .path()
        .trim_end_matches('/')
        .to_string();
    if [
        Protocol::ChatCompletions,
        Protocol::Responses,
        Protocol::AnthropicMessages,
    ]
    .iter()
    .any(|protocol| {
        *protocol != provider.protocol && path.ends_with(&format!("/{}", protocol.suffix()))
    }) {
        return Err("接口地址与所选协议不符；请使用服务根地址、/v1，或匹配协议的完整地址".into());
    }
    provider.models_url = provider
        .models_url
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(url) = &provider.models_url {
        validate_url(url)?;
    }
    if provider.extra_body.as_ref().is_some_and(|v| !v.is_object()) {
        return Err("额外请求参数必须是 JSON 对象".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for model in &mut provider.models {
        model.id = model.id.trim().into();
        model.name = model.name.trim().into();
        if model.id.is_empty()
            || model.id.len() > 256
            || model.id.chars().any(char::is_control)
            || !ids.insert(model.id.clone())
        {
            return Err("模型 ID 不能为空、重复或包含控制字符".into());
        }
        if model.name.is_empty() {
            model.name = model.id.clone();
        }
        if model.context_window_tokens == 0 || model.context_window_tokens > u32::MAX as u64 {
            return Err("上下文长度必须是有效的正整数".into());
        }
        if !matches!(model.tool_mode.as_str(), "native" | "content") {
            return Err("工具调用模式无效".into());
        }
        if model
            .reasoning_effort
            .as_deref()
            .is_some_and(|s| !matches!(s, "low" | "medium" | "high"))
        {
            return Err("推理强度无效".into());
        }
    }
    if require_models
        && !provider
            .models
            .iter()
            .any(|m| m.id == provider.default_model)
    {
        return Err("请添加模型，并选择服务商的默认模型".into());
    }
    Ok(())
}
fn merge_key(provider: &mut Provider, previous: Option<&Provider>, clear: bool) {
    provider.api_key = if clear {
        None
    } else {
        provider
            .api_key
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                previous
                    .filter(|old| {
                        old.base_url == provider.base_url && old.protocol == provider.protocol
                    })
                    .and_then(|old| old.api_key.clone())
            })
    };
}

pub fn save(global: &Path, mut input: SaveProvider) -> Result<RegistryView, String> {
    let _lock = crate::memory::store::lock_root(global).map_err(|e| e.to_string())?;
    let mut registry = load(global)?;
    check_revision(&registry, &input.revision)?;
    validate(&mut input.provider, true)?;
    if registry
        .providers
        .iter()
        .any(|p| p.id != input.provider.id && p.name.eq_ignore_ascii_case(&input.provider.name))
    {
        return Err("已有同名服务商，请填写可区分的名称".into());
    }
    let previous = registry
        .providers
        .iter()
        .find(|p| p.id == input.provider.id);
    merge_key(&mut input.provider, previous, input.clear_api_key);
    let new_default = selection(&input.provider.id, &input.provider.default_model);
    let old_default_for_provider = registry
        .default_selection
        .as_ref()
        .is_some_and(|key| key.starts_with(&format!("provider:{}:", input.provider.id)));
    if let Some(index) = registry
        .providers
        .iter()
        .position(|p| p.id == input.provider.id)
    {
        registry.providers[index] = input.provider;
    } else {
        registry.providers.push(input.provider);
    }
    if input.set_default || registry.default_selection.is_none() || old_default_for_provider {
        registry.default_selection = Some(new_default);
    }
    persist(global, &registry)
}
pub fn delete(global: &Path, input: &DeleteProvider) -> Result<RegistryView, String> {
    let _lock = crate::memory::store::lock_root(global).map_err(|e| e.to_string())?;
    let mut registry = load(global)?;
    check_revision(&registry, &input.revision)?;
    registry.providers.retain(|p| p.id != input.id);
    if registry
        .default_selection
        .as_ref()
        .is_some_and(|key| key.starts_with(&format!("provider:{}:", input.id)))
    {
        registry.default_selection = registry
            .providers
            .first()
            .map(|p| selection(&p.id, &p.default_model));
    }
    persist(global, &registry)
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConfiguredModel {
    pub provider: Provider,
    pub model: ModelDefinition,
}
impl std::fmt::Debug for ConfiguredModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfiguredModel")
            .field("provider", &self.provider.id)
            .field("model", &self.model.id)
            .finish()
    }
}
impl ConfiguredModel {
    pub fn config(&self) -> ModelConfig {
        let mut config = ModelConfig::builder()
            .protocol(self.provider.protocol.sdk())
            .base_url(endpoint(&self.provider).expect("validated provider endpoint"))
            .maybe_api_key(self.provider.api_key.clone())
            .model(&self.model.id)
            .stream(true)
            .thinking(self.model.thinking)
            .maybe_extra_body(self.provider.extra_body.clone())
            .build();
        config
            .reasoning_effort
            .clone_from(&self.model.reasoning_effort);
        config.context_window_tokens = Some(self.model.context_window_tokens);
        config.supports_images = self.model.supports_images;
        config.tool_calling = if self.model.tool_mode == "content" {
            ToolCallingMode::Tagged
        } else {
            ToolCallingMode::Native
        };
        config
    }
}
pub fn resolve(global: &Path, key: &str) -> Result<Option<ConfiguredModel>, String> {
    Ok(load(global)?.providers.into_iter().find_map(|provider| {
        let model = provider
            .models
            .iter()
            .find(|m| selection(&provider.id, &m.id) == key)?
            .clone();
        Some(ConfiguredModel { provider, model })
    }))
}
pub fn default_config(global: &Path) -> Result<Option<ModelConfig>, String> {
    match load(global)?.default_selection {
        Some(key) => resolve(global, &key)?
            .map(|m| m.config())
            .map(Some)
            .ok_or_else(|| "默认模型已不存在，请重新配置".into()),
        None => Ok(None),
    }
}
pub fn key_for_config(global: &Path, config: &ModelConfig) -> Option<String> {
    load(global)
        .ok()?
        .providers
        .into_iter()
        .find_map(|provider| {
            if endpoint(&provider).ok()?.as_str() != config.base_url
                || provider.protocol.sdk() != config.protocol
                || provider.api_key != config.api_key
            {
                return None;
            }
            provider
                .models
                .iter()
                .find(|model| model.id == config.model)
                .map(|model| selection(&provider.id, &model.id))
        })
}

pub struct ConfiguredCatalog {
    pub global: PathBuf,
    pub fallback: Arc<dyn ModelCatalog>,
}
impl ModelCatalog for ConfiguredCatalog {
    fn list(&self) -> ModelListFuture<'_> {
        Box::pin(async move {
            let registry = match load(&self.global) {
                Ok(registry) => registry,
                Err(error) => {
                    eprintln!("[models] {error}");
                    return Vec::new();
                }
            };
            if registry.providers.is_empty() {
                return self.fallback.list().await;
            }
            registry
                .providers
                .into_iter()
                .flat_map(|provider| {
                    provider.models.clone().into_iter().map(move |model| {
                        let mut entry = ModelEntry::new(
                            &model.name,
                            selection(&provider.id, &model.id),
                            &provider.name,
                        );
                        entry.configured = Some(ConfiguredModel {
                            provider: provider.clone(),
                            model,
                        });
                        entry
                    })
                })
                .collect()
        })
    }
}

/// Fetch uses a draft without persisting it. Blank keys reuse saved secrets only for the exact same endpoint/protocol.
pub async fn fetch_models(
    global: &Path,
    mut draft: Provider,
    clear_key: bool,
) -> Result<Vec<ModelDefinition>, String> {
    validate(&mut draft, false)?;
    let registry = load(global)?;
    let previous = registry.providers.iter().find(|p| p.id == draft.id);
    merge_key(&mut draft, previous, clear_key);
    let endpoint = if let Some(url) = &draft.models_url {
        url.clone()
    } else {
        let mut url = url::Url::parse(&endpoint(&draft)?).map_err(|e| e.to_string())?;
        let path = crate::models::models_endpoint(url.path());
        url.set_path(&path);
        url.to_string()
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(endpoint);
    if draft.protocol == Protocol::AnthropicMessages {
        request = request.header("anthropic-version", "2023-06-01");
    }
    if let Some(key) = &draft.api_key {
        request = if draft.protocol == Protocol::AnthropicMessages {
            request.header("x-api-key", key)
        } else {
            request.bearer_auth(key)
        };
    }
    let response = request
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("获取模型失败 ({status})；可以直接手动填写模型 ID"));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| e.without_url().to_string())?;
    let data = body
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or("模型接口未返回 data 数组；请手动填写模型 ID")?;
    let mut seen = std::collections::BTreeSet::new();
    Ok(data
        .iter()
        .filter_map(|value| {
            let id = value.get("id")?.as_str()?.to_string();
            if !seen.insert(id.clone()) {
                return None;
            }
            let tool_mode = crate::models::normalize_tool_mode(
                &id,
                value.get("owned_by").and_then(|v| v.as_str()).unwrap_or(""),
                value
                    .get("tool_mode")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            )
            .filter(|mode| matches!(mode.as_str(), "native" | "content"))
            .unwrap_or_else(default_tool_mode);
            let context_window_tokens = value
                .get("context_window_tokens")
                .or_else(|| value.get("context_length"))
                .and_then(serde_json::Value::as_u64)
                .filter(|n| *n > 0 && u32::try_from(*n).is_ok())
                .unwrap_or_else(default_context);
            let supports_images = crate::models::normalize_supports_images_from_value(&id, value);
            Some(ModelDefinition {
                name: value
                    .get("name")
                    .or_else(|| value.get("display_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&id)
                    .to_string(),
                id,
                tool_mode,
                context_window_tokens,
                supports_images,
                ..ModelDefinition::default()
            })
        })
        .collect())
}

/// Import the existing connection without exposing its key to the webview or changing config.toml.
pub fn import_legacy(global: &Path, expected: String) -> Result<RegistryView, String> {
    let settings = crate::settings::Settings::load(Some(&global.join("config.toml")), &|key| {
        std::env::var(key).ok()
    })
    .map_err(|e| e.to_string())?;
    if !settings.is_configured() {
        return Err("当前没有可导入的模型连接".into());
    }
    let protocol = match settings.protocol {
        ModelProtocol::ChatCompletions => Protocol::ChatCompletions,
        ModelProtocol::Responses => Protocol::Responses,
        ModelProtocol::AnthropicMessages => Protocol::AnthropicMessages,
    };
    let model = ModelDefinition {
        id: settings.model.clone(),
        name: settings.model.clone(),
        context_window_tokens: settings.context_window_tokens,
        supports_images: settings.supports_images,
        tool_mode: settings.tool_mode.unwrap_or_else(default_tool_mode),
        thinking: settings.thinking,
        reasoning_effort: Some(settings.reasoning_effort),
    };
    save(
        global,
        SaveProvider {
            revision: expected,
            provider: Provider {
                id: format!("imported-{}", chrono::Utc::now().timestamp_millis()),
                name: "原有模型连接".into(),
                protocol,
                base_url: settings.base_url,
                models_url: settings.models_url,
                api_key: settings.api_key,
                default_model: settings.model,
                models: vec![model],
                extra_body: settings.extra_body,
            },
            clear_api_key: false,
            set_default: true,
        },
    )
}

pub async fn command(
    global: &Path,
    action: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let value = match action {
        "get" => serde_json::to_value(get(global)?),
        "save" => serde_json::to_value(save(
            global,
            serde_json::from_value(input).map_err(|e| e.to_string())?,
        )?),
        "delete" => serde_json::to_value(delete(
            global,
            &serde_json::from_value(input).map_err(|e| e.to_string())?,
        )?),
        "import" => serde_json::to_value(import_legacy(
            global,
            input
                .get("revision")
                .and_then(|v| v.as_str())
                .ok_or("missing revision")?
                .into(),
        )?),
        "fetch" => {
            #[derive(Deserialize)]
            struct Draft {
                provider: Provider,
                #[serde(default)]
                clear_api_key: bool,
            }
            let draft: Draft = serde_json::from_value(input).map_err(|e| e.to_string())?;
            serde_json::to_value(fetch_models(global, draft.provider, draft.clear_api_key).await?)
        }
        _ => return Err("unknown model provider action".into()),
    };
    value.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "shirley-providers-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn provider(id: &str) -> Provider {
        Provider {
            id: id.into(),
            name: format!("Provider {id}"),
            base_url: format!("https://{id}.example/v1"),
            api_key: Some(format!("secret-{id}")),
            default_model: "same-model".into(),
            models: vec![ModelDefinition {
                id: "same-model".into(),
                name: "Same model".into(),
                ..ModelDefinition::default()
            }],
            ..Provider::default()
        }
    }
    fn write(global: &Path, provider: Provider, set_default: bool) -> RegistryView {
        save(
            global,
            SaveProvider {
                revision: get(global).unwrap().revision,
                provider,
                clear_api_key: false,
                set_default,
            },
        )
        .unwrap()
    }

    #[test]
    fn profile_crud_redacts_credentials_preserves_other_config_and_rejects_stale_edits() {
        let dir = Temp::new();
        std::fs::write(dir.0.join("config.toml"), "[runtime]\nmax_steps = 42\n").unwrap();
        let initial = get(&dir.0).unwrap();
        let first = write(&dir.0, provider("a"), true);
        let json = serde_json::to_string(&first).unwrap();
        assert!(!json.contains("secret-a"));
        assert!(first.providers[0].has_api_key);
        let stale = save(
            &dir.0,
            SaveProvider {
                revision: initial.revision,
                provider: provider("b"),
                clear_api_key: false,
                set_default: false,
            },
        );
        assert!(stale.is_err());
        let second = write(&dir.0, provider("b"), false);
        assert_eq!(second.providers.len(), 2);
        assert_eq!(
            default_config(&dir.0).unwrap().unwrap().api_key.as_deref(),
            Some("secret-a")
        );
        let after = delete(
            &dir.0,
            &DeleteProvider {
                revision: second.revision,
                id: "a".into(),
            },
        )
        .unwrap();
        assert_eq!(after.default_selection, Some(selection("b", "same-model")));
        assert_eq!(
            std::fs::read_to_string(dir.0.join("config.toml")).unwrap(),
            "[runtime]\nmax_steps = 42\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.0.join(FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn blank_key_is_preserved_only_for_the_same_provider_endpoint_and_can_be_cleared() {
        let dir = Temp::new();
        let first = write(&dir.0, provider("a"), true);
        let mut draft = first.providers[0].provider.clone();
        draft.name = "Renamed".into();
        let second = write(&dir.0, draft, false);
        assert!(second.providers[0].has_api_key);
        let mut changed = second.providers[0].provider.clone();
        changed.base_url = "https://new.example/v1".into();
        let third = write(&dir.0, changed, false);
        assert!(!third.providers[0].has_api_key);
        let mut changed = third.providers[0].provider.clone();
        changed.api_key = Some("replacement".into());
        let fourth = write(&dir.0, changed, false);
        let cleared = save(
            &dir.0,
            SaveProvider {
                revision: fourth.revision,
                provider: fourth.providers[0].provider.clone(),
                clear_api_key: true,
                set_default: false,
            },
        )
        .unwrap();
        assert!(!cleared.providers[0].has_api_key);
    }

    #[tokio::test]
    async fn duplicate_model_ids_across_providers_resolve_to_distinct_complete_connections() {
        let dir = Temp::new();
        write(&dir.0, provider("a"), true);
        let mut b = provider("b");
        b.protocol = Protocol::AnthropicMessages;
        b.models[0].context_window_tokens = 200_000;
        b.models[0].supports_images = true;
        b.models[0].tool_mode = "content".into();
        b.extra_body = Some(serde_json::json!({"vendor": "b"}));
        write(&dir.0, b, false);
        let catalog = ConfiguredCatalog {
            global: dir.0.clone(),
            fallback: Arc::new(crate::models::StaticCatalog::builtin()),
        };
        let entries = catalog.list().await;
        assert_ne!(entries[0].value, entries[1].value);
        let a = entries[0].configured.as_ref().unwrap().config();
        let b = entries[1].configured.as_ref().unwrap().config();
        assert_eq!(a.model, b.model);
        assert_eq!(a.base_url, "https://a.example/v1/chat/completions");
        assert_eq!(b.base_url, "https://b.example/v1/messages");
        assert_eq!(b.protocol, ModelProtocol::AnthropicMessages);
        assert_eq!(b.api_key.as_deref(), Some("secret-b"));
        assert_eq!(b.context_window_tokens, Some(200_000));
        assert!(b.supports_images);
        assert!(matches!(b.tool_calling, ToolCallingMode::Tagged));
        assert_eq!(a.extra_body, None);
        assert_eq!(key_for_config(&dir.0, &b), Some(entries[1].value.clone()));
        assert!(!format!("{entries:?}").contains("secret-"));
    }

    #[test]
    fn endpoint_normalization_and_invalid_profiles_fail_before_writing() {
        for (protocol, suffix) in [
            (Protocol::ChatCompletions, "chat/completions"),
            (Protocol::Responses, "responses"),
            (Protocol::AnthropicMessages, "messages"),
        ] {
            let mut p = provider("a");
            p.protocol = protocol;
            p.base_url = "https://a.example".into();
            assert_eq!(
                endpoint(&p).unwrap(),
                format!("https://a.example/v1/{suffix}")
            );
        }
        let dir = Temp::new();
        let mut invalid = provider("a");
        invalid.models[0].context_window_tokens = 0;
        assert!(
            save(
                &dir.0,
                SaveProvider {
                    revision: get(&dir.0).unwrap().revision,
                    provider: invalid,
                    clear_api_key: false,
                    set_default: true
                }
            )
            .is_err()
        );
        assert!(!dir.0.join(FILE).exists());
        let mut invalid = provider("a");
        invalid.base_url = "file:///etc/passwd".into();
        assert!(endpoint(&invalid).is_err());
        std::fs::write(dir.0.join(FILE), b"broken").unwrap();
        assert!(default_config(&dir.0).is_err());
    }

    #[test]
    fn selection_keys_encode_arbitrary_model_names_without_provider_collisions() {
        assert_eq!(
            selection("id", "a/b:中文 ~!*'()"),
            "provider:id:a%2Fb%3A%E4%B8%AD%E6%96%87+%7E%21*%27%28%29"
        );
    }

    #[cfg(feature = "server")]
    #[test]
    fn session_remembers_its_provider_after_default_changes_and_blocks_deleted_provider() {
        use crate::bootstrap::test_support::test_factory;
        use crate::interface::runtime::runtime_from_factory;
        let dir = Temp::new();
        write(&dir.0, provider("a"), true);
        let factory = Arc::new(test_factory(dir.0.clone()));
        let mut runtime = runtime_from_factory(Arc::clone(&factory)).unwrap();
        let name = runtime.sessions.active_name().unwrap().to_string();
        let binding = selection("a", "same-model");
        let config = resolve(&dir.0, &binding).unwrap().unwrap().config();
        runtime
            .sessions
            .active_mut()
            .select_model(config, Some(binding.clone()))
            .unwrap();
        write(&dir.0, provider("b"), true);
        // Restoring an existing binding does not use the new default service.
        let restored_agent = factory.build_agent(Vec::new()).unwrap();
        let sessions = crate::interface::session::SessionManager::with_factory(
            restored_agent.agent,
            Some(name),
            Arc::clone(&factory.session_catalog),
            Arc::clone(&factory),
            None,
            Some(restored_agent.memory),
            Some(restored_agent.skills),
            Some(restored_agent.todo),
            restored_agent.subagents,
        );
        assert_eq!(
            sessions.active().model_selection.as_deref(),
            Some(binding.as_str())
        );
        assert_eq!(
            sessions
                .active()
                .agent
                .as_ref()
                .unwrap()
                .model_config()
                .api_key
                .as_deref(),
            Some("secret-a")
        );
        let registry = get(&dir.0).unwrap();
        delete(
            &dir.0,
            &DeleteProvider {
                revision: registry.revision,
                id: "a".into(),
            },
        )
        .unwrap();
        assert!(sessions.active().model_availability().is_err());
        assert_eq!(
            factory
                .build_agent(Vec::new())
                .unwrap()
                .agent
                .model_config()
                .api_key
                .as_deref(),
            Some("secret-b")
        );
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn fetch_uses_correct_auth_and_does_not_save_draft_or_replace_model_metadata() {
        use axum::{Json, Router, http::HeaderMap, routing::get};
        let dir = Temp::new();
        let app = Router::new().route("/v1/models", get(|headers: HeaderMap| async move {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer private-key");
            Json(serde_json::json!({"data": [{"id":"a", "name":"Model A"}, {"id":"a"}, {"id":"b"}]}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut draft = provider("draft");
        draft.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        draft.api_key = Some("private-key".into());
        draft.models.clear();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let models = fetch_models(&dir.0, draft, false).await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].name, "Model A");
        assert!(!dir.0.join(FILE).exists());
        task.abort();
    }
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn child_request_uses_parent_selected_service_instead_of_factory_default() {
        use axum::{Json, Router, http::HeaderMap, routing::post};
        let dir = Temp::new();
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let request_log = Arc::clone(&captured);
        let app = Router::new().route("/v1/chat/completions", post(move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
            let log = Arc::clone(&request_log);
            async move {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer selected-service-key");
                log.lock().unwrap().push(body);
                Json(serde_json::json!({"id":"mock", "object":"chat.completion", "created":0, "model":"selected-model", "choices":[{"index":0,"message":{"role":"assistant","content":"child completed"},"finish_reason":"stop"}], "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let factory = Arc::new(crate::bootstrap::test_support::test_factory_with_base(
            dir.0.clone(),
            "http://127.0.0.1:9/v1/chat/completions",
            false,
        ));
        let built = factory.build_agent(Vec::new()).unwrap();
        let manager = built.subagents.clone().unwrap();
        let mut session = crate::interface::session::Session::new(
            built.agent,
            None,
            None,
            None,
            None,
            None,
            None,
            built.subagents,
        );
        let selected = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url(url)
            .model("selected-model")
            .api_key("selected-service-key")
            .stream(false)
            .build();
        session.select_model(selected, None).unwrap();
        let id = manager
            .dispatch(
                crate::subagent::SubagentKind::Spawn,
                None,
                "test child".into(),
                Some(Vec::new()),
                None,
                0,
            )
            .unwrap();
        let outcome = manager
            .wait(&id, Some(std::time::Duration::from_secs(3)))
            .await
            .unwrap();
        assert!(!outcome.timed_out);
        assert_eq!(
            outcome.info.status,
            crate::subagent::SubagentStatus::Completed
        );
        assert_eq!(captured.lock().unwrap()[0]["model"], "selected-model");
        task.abort();
    }
    #[test]
    fn conversation_model_binding_moves_and_deletes_with_its_log() {
        use crate::session::{FileSessionCatalog, SessionCatalog};
        let dir = Temp::new();
        let source = FileSessionCatalog::new(dir.0.join("source"));
        let target = FileSessionCatalog::new(dir.0.join("target"));
        let (entry, _) = source.create().unwrap();
        let source_dir = source.storage_dir().unwrap();
        let target_dir = target.storage_dir().unwrap();
        let binding_name = format!("{}.model.json", entry.name);
        let key = selection("a", "same-model");
        std::fs::write(
            source_dir.join(&binding_name),
            serde_json::to_vec(&key).unwrap(),
        )
        .unwrap();
        source.move_to(&entry.name, &target_dir).unwrap();
        assert!(!source_dir.join(&binding_name).exists());
        assert_eq!(
            std::fs::read_to_string(target_dir.join(&binding_name)).unwrap(),
            serde_json::to_string(&key).unwrap()
        );
        target.delete(&entry.name).unwrap();
        assert!(!target_dir.join(&binding_name).exists());
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn unauthenticated_anthropic_catalog_still_sends_version_header() {
        use axum::{Json, Router, http::HeaderMap, routing::get};
        let dir = Temp::new();
        let app = Router::new().route("/v1/models", get(|headers: HeaderMap| async move {
            assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
            assert!(headers.get("authorization").is_none());
            assert!(headers.get("x-api-key").is_none());
            Json(serde_json::json!({"data":[{"id":"example-model", "context_length":200_000, "tool_mode":"content"}]}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut draft = provider("anthropic");
        draft.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        draft.api_key = None;
        draft.protocol = Protocol::AnthropicMessages;
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let models = fetch_models(&dir.0, draft, false).await.unwrap();
        assert_eq!(models[0].context_window_tokens, 200_000);
        assert_eq!(models[0].tool_mode, "content");
        task.abort();
    }
}
