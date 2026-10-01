//! 系统提示词公开契约的回归测试。
//!
//! 锁的是这两条对外承诺（见 `docs/architecture.md`、`Agent.md` 第三节）：
//!
//! 1. `SystemPrompt` 既接受固定字符串，也接受**函数**，函数会拿到
//!    `SystemPromptContext`（当前工作目录）。
//! 2. `Agent` 的 builder 接受 `.system_prompt(...)` 与 `.working_dir(...)`，
//!    且函数形式的提示词在构造时就按工作目录解析。
//!
//! 这些断言只依赖 `agent_sdk` 的公开 API。

use agent_sdk::{Agent, ModelConfig, ModelProtocol, SystemPrompt, SystemPromptContext};
use std::path::PathBuf;

fn model_config() -> ModelConfig {
    ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url("http://localhost")
        .model("test")
        .build()
}

/// 固定字符串：不关心上下文，任何情况下都返回同一份文本。
#[test]
fn static_prompt_is_context_independent() {
    let prompt: SystemPrompt = "你是 Shirley".into();
    assert_eq!(prompt.resolve(&SystemPromptContext::default()), "你是 Shirley");
}

/// 函数形式：能读到 `SystemPromptContext.working_dir`。
#[test]
fn function_prompt_reads_working_dir() {
    let prompt = SystemPrompt::from(|ctx: &SystemPromptContext| {
        format!(
            "cwd={}",
            ctx.working_dir
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<none>".into())
        )
    });

    assert_eq!(prompt.resolve(&SystemPromptContext::default()), "cwd=<none>");
    let with_dir = SystemPromptContext {
        working_dir: Some(PathBuf::from("/work")),
    };
    assert_eq!(prompt.resolve(&with_dir), "cwd=/work");
}

/// builder 必须同时接受函数形式的提示词与工作目录，且能成功构造。
#[test]
fn builder_accepts_function_prompt_and_working_dir() {
    let agent = Agent::builder()
        .model_config(model_config())
        .system_prompt(|ctx: &SystemPromptContext| {
            format!("cwd:{:?}", ctx.working_dir)
        })
        .working_dir(PathBuf::from("/work"))
        .build();
    // 构造成功即证明类型契约成立（解析在 `Agent::new` 内完成）。
    let _ = agent;
}

/// 默认不传提示词时也能构造（空提示词 = 不置顶 System 消息）。
#[test]
fn builder_defaults_to_empty_prompt() {
    let _ = Agent::builder().model_config(model_config()).build();
}
