//! 应用侧的系统提示词构造。
//!
//! SDK 只提供"提示词可以是函数、并会拿到运行时上下文"的机制
//! （`shirley_agent_sdk::SystemPrompt` / `SystemPromptContext`）。**拼什么内容**属于
//! 业务决策，落在这里。
//!
//! **文案与代码分离**：提示词正文维护在 `prompts/*.md`（编译期 `include_str!` 内嵌），
//! 代码只负责"选模板 + 填占位符"。目前拼三块：
//!
//! 1. 角色与工作边界（`prompts/system.md` 的固定文案）。
//! 2. 当前工作目录——`plan.md` 里记的痛点：AI 不知道工作区就会从文件系统根
//!    目录开始乱找，白烧 token。把根目录明确告诉它，收敛搜索范围。
//! 3. 项目指南文件（`Agent.md`）——告诉 AI"这个项目怎么跑、代码怎么组织"。

use std::path::{Path, PathBuf};

use shirley_agent_sdk::{SystemPrompt, SystemPromptContext};

use crate::prompt_template::render as render_template;

/// 项目指南文件名。放在工作区根目录，作为 Agent 的工作参考。
pub const GUIDE_FILE_NAME: &str = "Agent.md";

/// 系统提示词模板（角色 / 工作边界 / 工作区 / 项目指南）。
const SYSTEM_TEMPLATE: &str = include_str!("../prompts/system.md");
/// 项目指南存在时的内联模板。
const GUIDE_TEMPLATE: &str = include_str!("../prompts/project_guide.md");
/// 缺少项目指南时的提示模板。
const GUIDE_MISSING_TEMPLATE: &str = include_str!("../prompts/project_guide_missing.md");

/// 解析 coding agent 的工作区根目录。
///
/// 优先读环境变量 `SHIRLEY_WORKSPACE`（与 `bash` 工具一致，避免两处漂移），
/// 否则退回当前目录。路径会做 canonicalize；失败则原样返回，交给提示词如实描述。
pub fn workspace_root() -> PathBuf {
    let raw = std::env::var("SHIRLEY_WORKSPACE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// 读取工作区里的项目指南（`Agent.md`）。不存在或读失败都返回 `None`。
pub fn guide_path(root: &Path) -> PathBuf {
    root.join(GUIDE_FILE_NAME)
}

fn read_guide(root: &Path) -> Option<String> {
    let path = root.join(GUIDE_FILE_NAME);
    let content = std::fs::read_to_string(path).ok()?;
    (!content.trim().is_empty()).then_some(content)
}

/// 构造应用侧的系统提示词。
///
/// 返回 [`SystemPrompt`]，其函数在每次解析时读取当前工作目录与 `Agent.md`。
/// 这样压缩重建时重新生成的系统提示词，始终反映最新的项目状态。
pub fn build(working_dir: PathBuf) -> SystemPrompt {
    SystemPrompt::from(move |context: &SystemPromptContext| {
        let root = context
            .working_dir
            .clone()
            .unwrap_or_else(|| working_dir.clone());
        render(&root)
    })
}

/// 把角色、工作目录、项目指南拼成最终文本。
///
/// 拆出来单独可测：不依赖闭包与运行时上下文。
fn render(root: &Path) -> String {
    // 项目指南块：有则内联原文，无则给出"先自行探索"的提示。
    let guide_block = match read_guide(root) {
        Some(guide) => render_template(
            GUIDE_TEMPLATE,
            &[("guide_file", GUIDE_FILE_NAME), ("guide", &guide)],
        ),
        None => render_template(GUIDE_MISSING_TEMPLATE, &[("guide_file", GUIDE_FILE_NAME)]),
    };

    let working_dir = root.display().to_string();
    render_template(
        SYSTEM_TEMPLATE,
        &[("working_dir", &working_dir), ("project_guide", &guide_block)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_role_and_working_dir() {
        let dir = tempfile_dir("render_role");
        let text = render(&dir);
        assert!(text.contains("You are Shirley"), "应包含角色设定: {text}");
        assert!(
            text.contains(&dir.display().to_string()),
            "应包含工作目录: {text}"
        );
    }

    #[test]
    fn render_instructs_todo_ledger_usage() {
        // 多步任务应先拆解、并用 todo 账本记录每步状态与结论。
        // 这条断言防止以后改提示词时把这段指令弄丢。
        let dir = tempfile_dir("render_todo");
        let text = render(&dir);
        assert!(text.contains("`todo` tool"), "应指示使用 todo 工具: {text}");
        assert!(text.contains("break it down"), "应先拆解任务: {text}");
    }

    #[test]
    fn render_keeps_preamble_in_same_message_as_tool_calls() {
        // 旁白必须与工具调用写在**同一条** assistant 消息里，不能拆成两轮：
        // 无 tool_calls 的消息会结束本轮（runtime 的 is_finished 判定），
        // 把旁白单独发出去会让 ReAct 循环在探索中途"莫名停止"。
        // 这条断言防止以后改提示词时把这段指令弄丢。
        let dir = tempfile_dir("render_content_first");
        let text = render(&dir);
        assert!(
            text.contains("SAME assistant message"),
            "应要求旁白与工具调用同条消息: {text}"
        );
        assert!(
            text.contains("ends your turn"),
            "应说明无 tool_calls 的消息会结束本轮: {text}"
        );
    }

    #[test]
    fn render_includes_guide_when_present() {
        let dir = tempfile_dir("render_guide");
        std::fs::write(dir.join(GUIDE_FILE_NAME), "# 项目说明\n先跑 cargo test").unwrap();
        let text = render(&dir);
        assert!(text.contains("先跑 cargo test"), "应内联 Agent.md: {text}");
    }

    #[test]
    fn render_instructs_tool_environment_is_authoritative() {
        // 业务侧纠偏（原 SDK 提示词里的 figma 特判句，Step 7 迁到应用层）。
        // 防止以后改提示词时把它弄丢。
        let dir = tempfile_dir("render_tool_env");
        let text = render(&dir);
        assert!(
            text.contains("不要把模型服务自身的工作区"),
            "应说明工具执行环境才是权威: {text}"
        );
    }

    #[test]
    fn render_notes_missing_guide() {
        let dir = tempfile_dir("render_missing");
        let text = render(&dir);
        assert!(text.contains("未找到"), "缺少指南时应给出提示: {text}");
    }

    #[test]
    fn build_resolves_context_dir_over_default() {
        // 上下文里显式给了目录时，应以它为准，而不是构造时的默认目录。
        let default_dir = tempfile_dir("build_default");
        let ctx_dir = tempfile_dir("build_ctx");
        let prompt = build(default_dir);
        let resolved = prompt.resolve(&SystemPromptContext {
            working_dir: Some(ctx_dir.clone()),
        });
        assert!(
            resolved.contains(&ctx_dir.display().to_string()),
            "应使用上下文目录: {resolved}"
        );
    }

    #[test]
    fn build_falls_back_to_default_dir() {
        let default_dir = tempfile_dir("build_fallback");
        let prompt = build(default_dir.clone());
        let resolved = prompt.resolve(&SystemPromptContext { working_dir: None });
        assert!(
            resolved.contains(&default_dir.display().to_string()),
            "无上下文时应回退默认目录: {resolved}"
        );
    }

    /// 生成一个唯一的临时目录，避免测试间相互污染。
    fn tempfile_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shirley_prompt_{tag}_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
}
