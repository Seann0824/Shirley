//! 应用侧的系统提示词构造。
//!
//! SDK 只提供"提示词可以是函数、并会拿到运行时上下文"的机制
//! （`shirley_agent_sdk::SystemPrompt` / `SystemPromptContext`）。**拼什么内容**属于
//! 业务决策，落在这里。
//!
//! 目前拼三块：
//!
//! 1. 角色与工作边界（固定文案）。
//! 2. 当前工作目录——`plan.md` 里记的痛点：AI 不知道工作区就会从文件系统根
//!    目录开始乱找，白烧 token。把根目录明确告诉它，收敛搜索范围。
//! 3. 项目指南文件（`Agent.md`）——告诉 AI"这个项目怎么跑、代码怎么组织"。

use std::path::{Path, PathBuf};

use shirley_agent_sdk::{SystemPrompt, SystemPromptContext};

/// 项目指南文件名。放在工作区根目录，作为 Agent 的工作参考。
pub const GUIDE_FILE_NAME: &str = "Agent.md";

/// 系统提示词里固定的角色 / 边界部分。
const ROLE: &str = "
You are Shirley, base on Englife-1.0, You are runing as coding agent in the Shirley CLI on user's computer.
Minimize unnecessary context usage when inspecting files.

To read a file, use the `read_file` tool rather than `cat` in bash:
- It enforces a workspace boundary and per-call line/byte limits, so it cannot flood the context.
- It returns line-numbered text; continue with `start_line` when the result says the content was truncated.
- For large or unknown-size files, first locate relevant content with commands such as `rg`, `grep`, `find`, or `wc -l`, then read only the relevant ranges via `read_file`.
- Expand the inspected range incrementally only when more context is needed.
- Reserve `cat` for small files whose full contents are genuinely needed.

Treat terminal output as part of the limited model context. Avoid commands that produce large amounts of irrelevant output.

For any multi-step task, first break it down with the `todo` tool: set the goal and the step checklist before you act, keep exactly one step in progress, and record what each step found (conclusions, locations, decisions, open questions) so you never redo work. The ledger survives context compaction.
";

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
    let mut prompt = String::new();
    prompt.push_str(ROLE);
    prompt.push_str("\n\n");

    prompt.push_str("# 工作区\n");
    prompt.push_str(&format!(
        "你的工作目录（工作区根）是：{}\n\
         所有文件路径都应相对于该目录理解；探索项目时请从这里开始，不要跑到文件系统其他位置。\n",
        root.display()
    ));

    match read_guide(root) {
        Some(guide) => {
            prompt.push_str("\n# 项目指南（");
            prompt.push_str(GUIDE_FILE_NAME);
            prompt.push_str("）\n");
            prompt.push_str(&guide);
            prompt.push('\n');
        }
        None => {
            prompt.push_str(&format!(
                "\n# 项目指南\n未找到 {GUIDE_FILE_NAME}，请先自行探索项目结构再动手。\n"
            ));
        }
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_role_and_working_dir() {
        let dir = tempfile_dir("render_role");
        let text = render(&dir);
        assert!(text.contains(ROLE), "应包含角色设定");
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
    fn render_includes_guide_when_present() {
        let dir = tempfile_dir("render_guide");
        std::fs::write(dir.join(GUIDE_FILE_NAME), "# 项目说明\n先跑 cargo test").unwrap();
        let text = render(&dir);
        assert!(text.contains("先跑 cargo test"), "应内联 Agent.md: {text}");
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
