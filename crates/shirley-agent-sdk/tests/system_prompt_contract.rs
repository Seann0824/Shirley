//! Regression tests for the public system-prompt contract.
//!
//! These lock two public promises (see `docs/architecture.md` and section 3 of `Agent.md`):
//!
//! 1. `SystemPrompt` accepts both a fixed string and a **function**; the function receives
//!    a `SystemPromptContext` (the current working directory).
//! 2. The `Agent` builder accepts `.system_prompt(...)` and `.working_dir(...)`,
//!    and a function-form prompt is resolved against the working directory at construction time.
//!
//! These assertions rely only on the public API of `shirley_agent_sdk`.

use shirley_agent_sdk::{Agent, ModelConfig, ModelProtocol, SystemPrompt, SystemPromptContext};
use std::path::PathBuf;

fn model_config() -> ModelConfig {
    ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url("http://localhost")
        .model("test")
        .build()
}

/// Fixed string: ignores the context and returns the same text in every case.
#[test]
fn static_prompt_is_context_independent() {
    let prompt: SystemPrompt = "You are Shirley".into();
    assert_eq!(
        prompt.resolve(&SystemPromptContext::default()),
        "You are Shirley"
    );
}

/// Function form: can read `SystemPromptContext.working_dir`.
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

    assert_eq!(
        prompt.resolve(&SystemPromptContext::default()),
        "cwd=<none>"
    );
    let with_dir = SystemPromptContext {
        working_dir: Some(PathBuf::from("/work")),
    };
    assert_eq!(prompt.resolve(&with_dir), "cwd=/work");
}

/// The builder must accept both a function-form prompt and a working directory, and construct successfully.
#[test]
fn builder_accepts_function_prompt_and_working_dir() {
    let agent = Agent::builder()
        .model_config(model_config())
        .system_prompt(|ctx: &SystemPromptContext| format!("cwd:{:?}", ctx.working_dir))
        .working_dir(PathBuf::from("/work"))
        .build();
    // A successful construction proves the type contract holds (resolution happens inside `Agent::new`).
    let _ = agent.unwrap();
}

/// It also constructs when no prompt is passed (empty prompt = no pinned System message).
#[test]
fn builder_defaults_to_empty_prompt() {
    let _ = Agent::builder()
        .model_config(model_config())
        .build()
        .unwrap();
}
