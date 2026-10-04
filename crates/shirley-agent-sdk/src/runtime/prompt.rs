//! 系统提示词：支持固定字符串，也支持按运行时上下文动态生成。
//!
//! 之所以要"函数形式"，是因为 system prompt 需要把运行时信息拼进去——
//! 最典型的是 coding agent 的**当前工作目录**（`plan.md`：AI 不知道工作区
//! 就会从文件系统根目录开始找，白烧 token），以及项目自带的**指南文件**
//! （本仓库里是 `Agent.md`）。
//!
//! 这些信息在 `Agent` 构造时才知道，且希望"每次重建都现算"，所以把提示词
//! 建模成一个 `Fn(&SystemPromptContext) -> String`。调用方既可以图省事直接
//! 传字符串，也可以传一个函数去读取工作目录、拼项目指南。

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

/// 构建系统提示词时可用的运行时上下文。
///
/// 目前只携带"当前工作目录"。后续如果需要注入更多运行时信息
/// （例如平台、可用工具摘要），在这里加字段即可，函数签名不用变。
#[derive(Debug, Clone, Default)]
pub struct SystemPromptContext {
    /// 当前工作目录（coding agent 的工作范围根）。
    ///
    /// `None` 表示调用方没有提供——提示词函数需要自行处理这种情况，
    /// 不要假定它一定存在。
    pub working_dir: Option<PathBuf>,
}

/// 系统提示词。
///
/// 既能是固定字符串（`From<String>` / `From<&str>`），也能是一个函数
/// （`From<F: Fn(&SystemPromptContext) -> String>`）。函数形式让提示词可以
/// 按运行时环境动态生成，例如把当前工作目录、项目指南（`Agent.md`）拼进去。
///
/// 之所以用 `Arc<dyn Fn>` 而不是泛型参数：`Agent` 需要把它存进字段，并在
/// 压缩重建时反复调用。泛型会把类型参数传染给整个 `Agent`，得不偿失。
/// 函数必须是 `Send + Sync`，因为 `run_stream` 返回的流是 `Send` 的。
#[derive(Clone)]
pub struct SystemPrompt {
    resolve: Arc<dyn Fn(&SystemPromptContext) -> String + Send + Sync>,
}

impl SystemPrompt {
    /// 按当前上下文解析出最终的系统提示词文本。
    pub fn resolve(&self, context: &SystemPromptContext) -> String {
        (self.resolve)(context)
    }
}

impl Default for SystemPrompt {
    /// 默认是空提示词——`Agent` 会据此判断"没有系统提示词"，不置顶 System 消息。
    fn default() -> Self {
        Self::from(String::new())
    }
}

impl fmt::Debug for SystemPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SystemPrompt")
            .field("resolve", &"<fn>")
            .finish()
    }
}

impl From<String> for SystemPrompt {
    fn from(prompt: String) -> Self {
        Self {
            resolve: Arc::new(move |_| prompt.clone()),
        }
    }
}

impl From<&str> for SystemPrompt {
    fn from(prompt: &str) -> Self {
        Self::from(prompt.to_owned())
    }
}

impl<F> From<F> for SystemPrompt
where
    F: Fn(&SystemPromptContext) -> String + Send + Sync + 'static,
{
    fn from(resolve: F) -> Self {
        Self {
            resolve: Arc::new(resolve),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: Option<&str>) -> SystemPromptContext {
        SystemPromptContext {
            working_dir: dir.map(PathBuf::from),
        }
    }

    #[test]
    fn static_string_ignores_context() {
        let prompt = SystemPrompt::from("你是 Shirley");
        assert_eq!(prompt.resolve(&ctx(None)), "你是 Shirley");
        assert_eq!(prompt.resolve(&ctx(Some("/tmp"))), "你是 Shirley");
    }

    #[test]
    fn str_slice_converts() {
        let prompt: SystemPrompt = "hello".into();
        assert_eq!(prompt.resolve(&ctx(None)), "hello");
    }

    #[test]
    fn function_receives_working_dir() {
        let prompt = SystemPrompt::from(|ctx: &SystemPromptContext| {
            format!(
                "cwd={}",
                ctx.working_dir
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<none>".into())
            )
        });
        assert_eq!(prompt.resolve(&ctx(Some("/work"))), "cwd=/work");
        assert_eq!(prompt.resolve(&ctx(None)), "cwd=<none>");
    }

    #[test]
    fn function_item_converts() {
        fn build(ctx: &SystemPromptContext) -> String {
            format!("dir:{:?}", ctx.working_dir)
        }
        let prompt = SystemPrompt::from(build);
        assert!(prompt.resolve(&ctx(Some("/a"))).contains("/a"));
    }

    #[test]
    fn default_resolves_to_empty() {
        let prompt = SystemPrompt::default();
        assert!(prompt.resolve(&ctx(Some("/a"))).is_empty());
    }

    #[test]
    fn resolve_is_repeatable() {
        // 压缩重建会反复调用 resolve，结果必须稳定、可重入。
        let prompt = SystemPrompt::from(|_: &SystemPromptContext| "stable".to_string());
        assert_eq!(prompt.resolve(&ctx(None)), prompt.resolve(&ctx(None)));
    }
}
