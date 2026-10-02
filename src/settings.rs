//! 应用侧的配置装载（方案 A：配置文件 + 环境变量覆盖）。
//!
//! 动机（`plan.md`）：面向普通用户时，"改 `.env` 才能换模型服务"太程序员化。
//! 把"配置从哪来"和"配置怎么用"解耦——`main.rs` 只消费最终结果，
//! 不再直接 `std::env::var`。
//!
//! **这是应用层的事，不进 SDK**：SDK 只认 `ModelConfig`，不关心它是从
//! 环境变量、配置文件还是向导来的。
//!
//! # 优先级（后者覆盖前者）
//!
//! 1. 内置默认（协议 `ChatCompletions`、模型 `deepseek-v4.1-flash`、
//!    上下文窗口 `104858 >> 1`）——与旧 `main.rs` 行为一致。
//! 2. 全局配置：`<config_dir>/shirley/config.toml`。
//! 3. 工作区配置：`<workspace_root>/.shirley/config.toml`。
//! 4. 环境变量：`LOCAL_API_KEY` / `LOCAL_BASE_URL` / `LOCAL_MODELS_URL` /
//!    `LOCAL_MODEL` / `LOCAL_PROTOCOL` / `LOCAL_CONTEXT_WINDOW_TOKENS`。
//!
//! 环境变量放在最后，是为了兼容既有的 `.env` 习惯：老用户什么都不用改，
//! 新用户写配置文件即可。两者都留，迁移是渐进的。
//!
//! # 不属于这里的东西
//!
//! `SHIRLEY_WORKSPACE` **刻意不收编**：它是"这次在哪个项目跑"（会话级），
//! 与"用户是谁 / 用哪个模型服务"（用户级）不是一类。混在一起会让配置文件
//! 在项目间不可移植。工作区根目录仍由 `prompt::workspace_root()` 单独解析。

use std::path::{Path, PathBuf};

use agent_sdk::ModelProtocol;
use serde::{Deserialize, Serialize};

/// 全局配置相对 `config_dir` 的位置。
const GLOBAL_CONFIG_REL: &str = "shirley/config.toml";
/// 工作区配置相对工作区根目录的位置。
const WORKSPACE_CONFIG_REL: &str = ".shirley/config.toml";

/// 内置默认模型：与旧 `main.rs` 的硬编码保持一致，升级不改变默认行为。
const DEFAULT_MODEL: &str = "deepseek-v4.1-flash";
/// 内置默认上下文窗口：`104858 >> 1`（旧 `main.rs` 的取值）。
const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 104858 >> 1;

/// 配置文件的结构（也是每一层覆盖的载体）。
///
/// 所有字段都是 `Option`：`None` 表示"这一层没管这个键"，合并时让位给更低
/// 优先级的那层。`deny_unknown_fields` 让拼错的键直接报错，而不是被静默忽略
/// ——配置写错了要吵，不要假装没看见。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileSettings {
    pub provider: ProviderSettings,
}

/// 模型服务连接配置（配置文件里的 `[provider]` 表）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderSettings {
    /// 协议名，见 [`parse_protocol`]。
    pub protocol: Option<String>,
    /// chat completions 完整地址，例如 `http://127.0.0.1:8788/v1/chat/completions`。
    pub base_url: Option<String>,
    /// 密钥。缺失时按"无鉴权"处理（本地服务常见）。
    pub api_key: Option<String>,
    /// 模型列表接口；缺省时从 `base_url` 推导（见 `models::models_endpoint`）。
    pub models_url: Option<String>,
    /// 请求用的模型标识。
    pub model: Option<String>,
    /// 上下文窗口 token 数。
    pub context_window_tokens: Option<u64>,
}

impl FileSettings {
    /// 从 TOML 文本解析。空文本视为"空配置"。
    pub fn parse(text: &str, path: &Path) -> Result<Self, SettingsError> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        toml::from_str(text).map_err(|e| SettingsError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    /// 把当前文件设置写回 `path`（原子替换：先写临时文件再 `rename`）。
    ///
    /// 写入会**整体覆盖**该文件——调用方应先把已有内容读出来、改字段、再写回，
    /// 否则会丢掉文件里其它键。目录不存在时自动创建。
    ///
    /// 写完后把文件权限收紧到 `0600`（仅所有者可读写）：配置文件里可能有
    /// `api_key`，默认 `0644` 会把密钥暴露给同机其他用户。收紧失败不视为致命
    /// 错误——某些文件系统（如部分挂载卷）不支持 chmod，能写下去更重要。
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| SettingsError::Parse {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        restrict_permissions(path);
        Ok(())
    }

    /// 读取文件；不存在返回 `None`（缺配置不是错误，是"这层没说话"）。
    fn load_file(path: &Path) -> Result<Option<Self>, SettingsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(Self::parse(&text, path)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SettingsError::Io(e)),
        }
    }
}

/// 合并后的最终配置。字段都已落定，`main.rs` 直接消费。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub protocol: ModelProtocol,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models_url: Option<String>,
    pub model: String,
    pub context_window_tokens: u64,
}

/// 配置装载错误。
///
/// 沿用项目统一的展示格式 `[前缀]: 详情`（见 `Agent.md`）。
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("[配置读取失败]: {0}")]
    Io(#[from] std::io::Error),

    #[error("[配置解析失败] {path}: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("[配置缺失]: {0}")]
    Missing(String),

    #[error("[配置非法]: {0}")]
    Invalid(String),
}

impl Settings {
    /// 按优先级链装载配置。
    ///
    /// `env` 是环境变量读取器（注入以便测试，不打全局 env）。
    /// `global_path` 为 `None` 时跳过全局层（例如拿不到 `config_dir`）。
    pub fn load(
        workspace_root: &Path,
        global_path: Option<&Path>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, SettingsError> {
        let global = match global_path {
            Some(path) => FileSettings::load_file(path)?,
            None => None,
        };
        let workspace = FileSettings::load_file(&workspace_root.join(WORKSPACE_CONFIG_REL))?;
        let env_layer = env_layer(env);

        // 低优先级在前，逐层覆盖。文件层取其 `[provider]` 表，env 层本就是一个表。
        let merged = merge_all([
            global.map(|s| s.provider),
            workspace.map(|s| s.provider),
            Some(env_layer),
        ]);
        Self::finalize(merged)
    }

    /// 便捷入口：使用真实的 `config_dir` 与进程环境变量。
    pub fn load_default(workspace_root: &Path) -> Result<Self, SettingsError> {
        let global = dirs::config_dir().map(|dir| dir.join(GLOBAL_CONFIG_REL));
        Self::load(workspace_root, global.as_deref(), &|key| std::env::var(key).ok())
    }

    /// 把合并结果落成最终配置，补齐默认值并校验必填项。
    fn finalize(merged: ProviderSettings) -> Result<Self, SettingsError> {
        let base_url = merged
            .base_url
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| SettingsError::Missing(missing_base_url_hint()))?;

        let protocol = match &merged.protocol {
            Some(name) => parse_protocol(name)?,
            None => ModelProtocol::ChatCompletions,
        };

        let context_window_tokens = match merged.context_window_tokens {
            Some(0) => {
                return Err(SettingsError::Invalid(
                    "context_window_tokens 必须大于 0".into(),
                ))
            }
            Some(n) => n,
            None => DEFAULT_CONTEXT_WINDOW_TOKENS,
        };

        let model = merged
            .model
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());

        Ok(Self {
            protocol,
            base_url,
            api_key: merged.api_key.filter(|v| !v.is_empty()),
            models_url: merged.models_url.filter(|v| !v.trim().is_empty()),
            model,
            context_window_tokens,
        })
    }
}

/// 把文件权限收紧到仅所有者可读写（`0600`）。非 Unix 平台跳过。
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// 把一组 provider 配置写入配置文件（**先读后改再写**，只覆盖传入的字段，
/// 文件里其它键原样不动）。返回写入的路径，供上层反馈给用户。
///
/// 这是 `/login` 落盘的入口。路径由调用方给出：默认是工作区
/// `<root>/.shirley/config.toml`（`/login` 的语义是"当前项目用哪个模型服务"，
/// 落到项目里可随项目走），测试可注入临时路径。需要全局生效时换一个路径即可。
pub fn save_provider(
    path: &Path,
    provider: ProviderSettings,
) -> Result<PathBuf, SettingsError> {
    let mut existing = FileSettings::load_file(path)?.unwrap_or_default();
    merge_into(&mut existing.provider, provider);
    existing.save(path)?;
    Ok(path.to_path_buf())
}

/// 环境变量层：把进程环境映射成一个 [`ProviderSettings`]。
fn env_layer(env: &dyn Fn(&str) -> Option<String>) -> ProviderSettings {
    ProviderSettings {
        protocol: env("LOCAL_PROTOCOL"),
        base_url: env("LOCAL_BASE_URL"),
        api_key: env("LOCAL_API_KEY"),
        models_url: env("LOCAL_MODELS_URL"),
        model: env("LOCAL_MODEL"),
        context_window_tokens: env("LOCAL_CONTEXT_WINDOW_TOKENS").and_then(|v| v.parse().ok()),
    }
}

/// 逐层覆盖：高优先级的 `Some` 压过低优先级的 `Some`。
///
/// 输入按**优先级从低到高**排列；`None` 层直接跳过。
fn merge_all(layers: [Option<ProviderSettings>; 3]) -> ProviderSettings {
    let mut merged = ProviderSettings::default();
    for layer in layers.into_iter().flatten() {
        merge_into(&mut merged, layer);
    }
    merged
}

fn merge_into(base: &mut ProviderSettings, over: ProviderSettings) {
    macro_rules! take {
        ($($field:ident),* $(,)?) => {
            $(if over.$field.is_some() { base.$field = over.$field; })*
        };
    }
    take!(
        protocol,
        base_url,
        api_key,
        models_url,
        model,
        context_window_tokens,
    );
}

/// 协议名解析。接受常见别名，未知名字报错（不静默回退）。
fn parse_protocol(name: &str) -> Result<ModelProtocol, SettingsError> {
    let normalized = name.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "chat_completions" | "chatcompletions" | "openai" => Ok(ModelProtocol::ChatCompletions),
        "responses" => Ok(ModelProtocol::Responses),
        "anyhtopic_messages" | "anthropic" | "messages" => Ok(ModelProtocol::AnyhtopicMessages),
        other => Err(SettingsError::Invalid(format!(
            "未知协议 `{other}`，可选：chat_completions / responses / anyhtopic_messages"
        ))),
    }
}

/// 缺少 base_url 时的提示，把"该去哪配"直接说清楚。
fn missing_base_url_hint() -> String {
    format!(
        "未找到模型服务地址（base_url）。请在环境变量 `LOCAL_BASE_URL`、\
         工作区 `{WORKSPACE_CONFIG_REL}` 或全局配置的 `[provider] base_url` 中至少配置一处。\n\
         例如：\n  [provider]\n  base_url = \"http://127.0.0.1:8788/v1/chat/completions\""
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        }
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shirley_settings_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn env_supplies_base_url_and_defaults_fill_rest() {
        let root = temp_root("env_only");
        let settings =
            Settings::load(&root, None, &env_of(&[("LOCAL_BASE_URL", "http://x/v1/chat/completions")]))
                .unwrap();
        assert_eq!(settings.base_url, "http://x/v1/chat/completions");
        assert_eq!(settings.protocol, ModelProtocol::ChatCompletions);
        assert_eq!(settings.model, DEFAULT_MODEL);
        assert_eq!(settings.context_window_tokens, DEFAULT_CONTEXT_WINDOW_TOKENS);
        assert_eq!(settings.api_key, None);
    }

    #[test]
    fn missing_base_url_is_actionable_error() {
        let root = temp_root("missing");
        let err = Settings::load(&root, None, &env_of(&[])).unwrap_err();
        assert!(matches!(err, SettingsError::Missing(_)));
        assert!(err.to_string().contains("LOCAL_BASE_URL"), "应提示去哪配: {err}");
    }

    #[test]
    fn workspace_file_overrides_global() {
        let root = temp_root("ws_over_global");
        let global_dir = temp_root("global_dir");
        let global = global_dir.join("config.toml");
        std::fs::write(
            &global,
            "[provider]\nbase_url = \"http://global\"\nmodel = \"g-model\"\n",
        )
        .unwrap();
        let ws_dir = root.join(".shirley");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[provider]\nbase_url = \"http://workspace\"\n",
        )
        .unwrap();

        let settings = Settings::load(&root, Some(&global), &env_of(&[])).unwrap();
        // 工作区覆盖 base_url，全局的 model 保留。
        assert_eq!(settings.base_url, "http://workspace");
        assert_eq!(settings.model, "g-model");
    }

    #[test]
    fn env_overrides_files() {
        let root = temp_root("env_over_file");
        let ws_dir = root.join(".shirley");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[provider]\nbase_url = \"http://file\"\napi_key = \"file-key\"\n",
        )
        .unwrap();

        let settings = Settings::load(
            &root,
            None,
            &env_of(&[
                ("LOCAL_BASE_URL", "http://env"),
                ("LOCAL_MODEL", "env-model"),
            ]),
        )
        .unwrap();
        assert_eq!(settings.base_url, "http://env", "env 应覆盖文件");
        assert_eq!(settings.model, "env-model");
        // 文件里有、env 里没有的键应保留。
        assert_eq!(settings.api_key.as_deref(), Some("file-key"));
    }

    #[test]
    fn parses_provider_table_from_file() {
        let root = temp_root("parse_table");
        let ws_dir = root.join(".shirley");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[provider]\nprotocol = \"responses\"\nbase_url = \"http://x\"\ncontext_window_tokens = 128000\n",
        )
        .unwrap();
        let settings = Settings::load(&root, None, &env_of(&[])).unwrap();
        assert_eq!(settings.protocol, ModelProtocol::Responses);
        assert_eq!(settings.context_window_tokens, 128000);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let root = temp_root("unknown_key");
        let ws_dir = root.join(".shirley");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("config.toml"),
            "[provider]\nbase_url = \"http://x\"\nbas_url = \"typo\"\n",
        )
        .unwrap();
        let err = Settings::load(&root, None, &env_of(&[])).unwrap_err();
        assert!(matches!(err, SettingsError::Parse { .. }), "拼错的键应报错: {err}");
    }

    #[test]
    fn zero_context_window_is_rejected() {
        let root = temp_root("zero_ctx");
        let err = Settings::load(
            &root,
            None,
            &env_of(&[
                ("LOCAL_BASE_URL", "http://x"),
                ("LOCAL_CONTEXT_WINDOW_TOKENS", "0"),
            ]),
        )
        .unwrap_err();
        assert!(matches!(err, SettingsError::Invalid(_)));
    }

    #[test]
    fn unknown_protocol_is_rejected() {
        let root = temp_root("bad_proto");
        let err = Settings::load(
            &root,
            None,
            &env_of(&[
                ("LOCAL_BASE_URL", "http://x"),
                ("LOCAL_PROTOCOL", "gemini"),
            ]),
        )
        .unwrap_err();
        assert!(matches!(err, SettingsError::Invalid(_)));
    }

    #[test]
    fn save_provider_preserves_other_keys() {
        let root = temp_root("save_preserve");
        let path = root.join(".shirley/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 预置一个 models_url（本次不覆盖它）。
        std::fs::write(
            &path,
            "[provider]\nmodels_url = \"http://keep/v1/models\"\nbase_url = \"http://old\"\n",
        )
        .unwrap();

        save_provider(
            &path,
            ProviderSettings {
                base_url: Some("http://new".into()),
                api_key: Some("sk".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let saved = FileSettings::load_file(&path).unwrap().unwrap();
        assert_eq!(saved.provider.base_url.as_deref(), Some("http://new"));
        assert_eq!(saved.provider.api_key.as_deref(), Some("sk"));
        assert_eq!(
            saved.provider.models_url.as_deref(),
            Some("http://keep/v1/models"),
            "未覆盖的键应保留"
        );
    }

    #[test]
    fn save_provider_creates_missing_dirs() {
        let root = temp_root("save_dirs");
        let path = root.join("nested/deep/config.toml");
        save_provider(
            &path,
            ProviderSettings {
                base_url: Some("http://x".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(path.exists(), "应自动创建父目录");
    }

    #[test]
    fn saved_provider_round_trips_through_load() {
        let root = temp_root("save_roundtrip");
        let path = root.join(".shirley/config.toml");
        save_provider(
            &path,
            ProviderSettings {
                base_url: Some("http://saved".into()),
                model: Some("m".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let settings = Settings::load(&root, None, &env_of(&[])).unwrap();
        assert_eq!(settings.base_url, "http://saved");
        assert_eq!(settings.model, "m");
    }

    #[test]
    fn empty_file_is_empty_layer() {
        let root = temp_root("empty_file");
        let ws_dir = root.join(".shirley");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(ws_dir.join("config.toml"), "   \n").unwrap();
        let settings =
            Settings::load(&root, None, &env_of(&[("LOCAL_BASE_URL", "http://x")])).unwrap();
        assert_eq!(settings.base_url, "http://x");
    }
}
