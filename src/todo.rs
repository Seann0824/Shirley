//! 任务账本（todo）：模型自己维护、跨上下文压缩存活的任务状态。
//!
//! **这是应用层能力，不是 SDK 基础能力**。账本属于"coding agent 这个产品"
//! 的功能取舍（记什么、怎么催、注入什么文案），SDK 只提供通用接缝
//! [`shirley_agent_sdk::ContextProvider`]——"每轮请求末尾追加一条 system"。
//! 本模块实现两件事：
//!
//! - [`TodoTool`]：暴露给模型的 `todo` 工具（模型自己决定何时更新）；
//! - [`TodoContextProvider`]：把账本渲染成一条 system，经 `ContextProvider`
//!   接缝**追加在每轮请求末尾**（不参与压缩、跨压缩存活、不动前缀缓存）。
//!
//! **为什么需要它**：压缩会把对话压成有损摘要，模型因此丢失"我做到哪了、
//! 已经知道什么"，于是重新探索、再压缩，形成正反馈回路。账本把"目标 / 步骤 /
//! 已得结论 / 待决问题"记在**对话之外**，压缩碰不到它。
//!
//! 账本不是"再摘要一遍"（那是二次损失）：写进去的是模型自己确认过的事实。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use shirley_agent_sdk::{ContextProvider, Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

/// 账本渲染成注入文本时的字符上限。
///
/// 账本本身也会占上下文，超限截断并显式标注（防它自己垄断上下文）。
const MAX_RENDER_CHARS: usize = 4000;

/// 连续多少轮没更新账本就注入 nag 提醒。
///
/// 取 3 与开源实现（learn-claude-code）一致：太早会打断正常节奏，
/// 太晚则漂移已经发生。
pub const TODO_NAG_AFTER_ROUNDS: usize = 3;

/// 任务账本的内存存储。
///
/// `Arc` 共享（注入提供者与 `todo` 工具各持一份），内部可变性走 `Mutex`。
/// 持久化留空（进程结束即失，与会话日志无关）。
pub struct TodoStore {
    inner: Mutex<TaskState>,
}

#[derive(Debug, Clone, Default)]
struct TaskState {
    goal: Option<String>,
    steps: Vec<Step>,
    findings: Vec<String>,
    open_questions: Vec<String>,
}

/// 一步的完成状态（对齐 Claude Code `TodoWrite` 的三态）。
///
/// `InProgress` 是"防漂移"的锚点：同一时刻至多一步处于此态，逼模型
/// 做完一件再开下一件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone)]
struct Step {
    text: String,
    status: TodoStatus,
}

/// 账本更新指令（`todo` 工具的入参）。
///
/// **补丁语义**：只处理显式提供的字段；未提供的字段保持原样。
/// - `goal` / `steps`：整体替换（`steps` 是清单，模型每轮重发全量，天然自纠）；
/// - `add_findings` / `add_open_questions`：追加；
/// - `clear`：重置整个账本。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<TodoStep>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_findings: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_open_questions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear: Option<bool>,
}

/// 清单里的一步。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoStep {
    pub text: String,
    /// 缺省视为 `pending`。三态见 [`TodoStatus`]；同一时刻至多一步 `in_progress`。
    #[serde(default)]
    pub status: TodoStatus,
}

impl TodoStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(TaskState::default()),
        }
    }

    /// 应用一次更新，返回更新后的账本渲染文本（空账本返回 `None`）。
    ///
    /// 校验失败（同一时刻超过一步 `in_progress`）返回 `Err`，且**不产生任何
    /// 副作用**——校验先于写入，避免半更新的脏账本。
    pub fn apply(&self, update: TodoUpdate) -> Result<Option<String>, ToolError> {
        if let Some(steps) = &update.steps {
            let in_progress = steps
                .iter()
                .filter(|step| step.status == TodoStatus::InProgress)
                .count();
            if in_progress > 1 {
                return Err(ToolError::ArgumentsError(
                    "at most one step may be in_progress at a time".into(),
                ));
            }
        }
        {
            let mut state = self.inner.lock().expect("todo lock poisoned");
            if update.clear == Some(true) {
                *state = TaskState::default();
            }
            if let Some(goal) = update.goal {
                let trimmed = goal.trim();
                state.goal = (!trimmed.is_empty()).then(|| trimmed.to_string());
            }
            if let Some(steps) = update.steps {
                state.steps = steps
                    .into_iter()
                    .map(|step| Step {
                        text: step.text,
                        status: step.status,
                    })
                    .collect();
            }
            if let Some(findings) = update.add_findings {
                state
                    .findings
                    .extend(findings.into_iter().filter(|f| !f.trim().is_empty()));
            }
            if let Some(questions) = update.add_open_questions {
                state
                    .open_questions
                    .extend(questions.into_iter().filter(|q| !q.trim().is_empty()));
            }
        }
        Ok(self.render())
    }

    /// 把账本渲染成注入文本；空账本返回 `None`（不注入空内容）。
    ///
    /// 用 XML 标签包裹（`plan.md`：标签形式能提升模型对结构信息的注意力）。
    pub fn render(&self) -> Option<String> {
        let state = self.inner.lock().expect("todo lock poisoned");
        let mut body = String::new();
        if let Some(goal) = &state.goal {
            body.push_str(&format!("<goal>{}</goal>\n", escape(goal)));
        }
        if !state.steps.is_empty() {
            body.push_str("<steps>\n");
            for step in &state.steps {
                let mark = match step.status {
                    TodoStatus::Pending => "[ ]",
                    TodoStatus::InProgress => "[>]",
                    TodoStatus::Completed => "[x]",
                };
                body.push_str(&format!("- {} {}\n", mark, escape(&step.text)));
            }
            body.push_str("</steps>\n");
        }
        if !state.findings.is_empty() {
            body.push_str("<findings>\n");
            for finding in &state.findings {
                body.push_str(&format!("- {}\n", escape(finding)));
            }
            body.push_str("</findings>\n");
        }
        if !state.open_questions.is_empty() {
            body.push_str("<open_questions>\n");
            for question in &state.open_questions {
                body.push_str(&format!("- {}\n", escape(question)));
            }
            body.push_str("</open_questions>\n");
        }
        if body.is_empty() {
            return None;
        }
        let mut rendered = format!("<task_state>\n{body}</task_state>");
        if rendered.chars().count() > MAX_RENDER_CHARS {
            rendered = rendered.chars().take(MAX_RENDER_CHARS).collect();
            rendered.push_str("\n... (task ledger truncated; keep it concise)");
        }
        Some(rendered)
    }
}

impl Default for TodoStore {
    fn default() -> Self {
        Self::new()
    }
}

/// 注入请求时给账本加的一行说明（模型据此知道这是什么、该怎么做）。
///
/// 与 [`TodoStore::render`] 分开：工具结果里不需要重复这段说明。
pub const TASK_STATE_HEADER: &str = "Your maintained task ledger (persists across context compaction). \
Update it with the `todo` tool. Do not repeat work already recorded as done.";

/// 模型连续多轮没更新账本时注入的催促提醒（nag）。
///
/// 与 `TASK_STATE_HEADER` 分开：这条只在**空账本**或**久未更新**时出现，
/// 是"冷启动护栏"——账本为空时没有 header 可注入，模型开局缺一条明确的
/// 触发指令，靠它补上。
pub const TODO_NAG_REMINDER: &str = "<reminder>You have gone several rounds without updating your task \
ledger. If this is a multi-step task, call the `todo` tool now: set the goal and the step \
checklist, mark the current step in_progress, and record what you have learned so far. Keep the \
ledger current as you work.</reminder>";

/// XML 文本转义：防止模型写入的 `<` / `&` 破坏标签结构。
fn escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `todo` 工具：模型维护自己任务账本的唯一入口。
///
/// 手写实现 `Tool`：状态（`Arc<TodoStore>`）由工具自己持有，与
/// [`TodoContextProvider`] 共享同一份 `Arc`。
pub struct TodoTool {
    store: Arc<TodoStore>,
    definition: ToolDefinition,
}

impl TodoTool {
    pub fn new(store: Arc<TodoStore>) -> Self {
        Self {
            store,
            definition: ToolDefinition {
                name: "todo".into(),
                description: "Maintain your own task ledger. For any multi-step task, break the \
                    work down and record it here BEFORE acting: set the goal and the step checklist. \
                    Mark exactly one step in_progress at a time as you work, mark it completed when \
                    done, and append findings (facts you have learned: file contents, command results, \
                    decisions) and open questions. The ledger is injected into every request and \
                    survives context compaction, so keeping it current prevents you from repeating \
                    work that was already done. Update it whenever you finish a step or learn \
                    something worth keeping."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "goal": {
                            "type": "string",
                            "description": "Set or replace the current task goal."
                        },
                        "steps": {
                            "type": "array",
                            "description": "Replace the full step checklist. Keep at most one step in_progress.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string", "description": "What this step does." },
                                    "status": {
                                        "type": "string",
                                        "enum": ["pending", "in_progress", "completed"],
                                        "default": "pending",
                                        "description": "Step state. At most one step may be in_progress at a time."
                                    }
                                },
                                "required": ["text"],
                                "additionalProperties": false
                            }
                        },
                        "add_findings": {
                            "type": "array",
                            "description": "Append facts you have learned (file contents, command results, decisions).",
                            "items": { "type": "string" }
                        },
                        "add_open_questions": {
                            "type": "array",
                            "description": "Append unresolved questions.",
                            "items": { "type": "string" }
                        },
                        "clear": {
                            "type": "boolean",
                            "description": "Reset the entire ledger."
                        }
                    },
                    "additionalProperties": false
                }),
            },
        }
    }
}

impl Tool for TodoTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        let store = self.store.clone();
        Box::pin(async move {
            let update: TodoUpdate = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            let rendered = store
                .apply(update)?
                .unwrap_or_else(|| "(task ledger is empty)".to_string());
            serde_json::to_value(format!("task ledger updated:\n{rendered}")).map_err(|error| {
                ToolError::ExecutionError(format!("failed to serialize tool result: {error}"))
            })
        })
    }
}

/// 每轮请求末尾注入任务账本的提供者。
///
/// 实现 SDK 的 [`ContextProvider`]：账本非空 → 注入 `header + 账本`；账本为空且
/// 连续多轮未建 → 注入一条 nag（冷启动护栏）。内部计数走 `AtomicUsize`（`context`
/// 取 `&self`）。
pub struct TodoContextProvider {
    store: Arc<TodoStore>,
    /// 距上次注入非空账本已过多少轮（每轮 = 一次组装请求）。
    rounds_since_todo: AtomicUsize,
}

impl TodoContextProvider {
    pub fn new(store: Arc<TodoStore>) -> Self {
        Self {
            store,
            rounds_since_todo: AtomicUsize::new(0),
        }
    }
}

impl ContextProvider for TodoContextProvider {
    fn context(&self) -> Option<String> {
        if let Some(ledger) = self.store.render() {
            self.rounds_since_todo.store(0, Ordering::Relaxed);
            Some(format!("{TASK_STATE_HEADER}\n\n{ledger}"))
        } else if self.rounds_since_todo.fetch_add(1, Ordering::Relaxed) >= TODO_NAG_AFTER_ROUNDS {
            Some(TODO_NAG_REMINDER.to_string())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_then_render_roundtrip() {
        let store = TodoStore::new();
        assert!(store.render().is_none());
        let rendered = store
            .apply(TodoUpdate {
                goal: Some("实现 todo 工具".into()),
                steps: Some(vec![
                    TodoStep {
                        text: "建模块".into(),
                        status: TodoStatus::Completed,
                    },
                    TodoStep {
                        text: "接运行时".into(),
                        status: TodoStatus::Pending,
                    },
                ]),
                ..Default::default()
            })
            .unwrap()
            .expect("非空账本应渲染");
        assert!(rendered.contains("<goal>实现 todo 工具</goal>"));
        assert!(rendered.contains("- [x] 建模块"));
        assert!(rendered.contains("- [ ] 接运行时"));
    }

    #[test]
    fn patch_only_touches_provided_fields() {
        let store = TodoStore::new();
        store
            .apply(TodoUpdate {
                goal: Some("目标".into()),
                ..Default::default()
            })
            .unwrap();
        store
            .apply(TodoUpdate {
                add_findings: Some(vec!["发现 A".into()]),
                ..Default::default()
            })
            .unwrap();
        let rendered = store.render().unwrap();
        assert!(rendered.contains("<goal>目标</goal>"), "goal 应保留");
        assert!(rendered.contains("- 发现 A"));
    }

    #[test]
    fn clear_resets_everything() {
        let store = TodoStore::new();
        store
            .apply(TodoUpdate {
                goal: Some("目标".into()),
                ..Default::default()
            })
            .unwrap();
        store
            .apply(TodoUpdate {
                clear: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert!(store.render().is_none());
    }

    #[test]
    fn rejects_multiple_in_progress() {
        let store = TodoStore::new();
        let err = store
            .apply(TodoUpdate {
                steps: Some(vec![
                    TodoStep {
                        text: "a".into(),
                        status: TodoStatus::InProgress,
                    },
                    TodoStep {
                        text: "b".into(),
                        status: TodoStatus::InProgress,
                    },
                ]),
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, ToolError::ArgumentsError(_)));
        assert!(store.render().is_none(), "校验失败不应产生副作用");
    }

    #[test]
    fn escape_protects_tag_structure() {
        let store = TodoStore::new();
        let rendered = store
            .apply(TodoUpdate {
                goal: Some("a < b & c > d".into()),
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        assert!(rendered.contains("a &lt; b &amp; c &gt; d"));
        assert!(!rendered.contains("a < b"));
    }

    #[test]
    fn provider_injects_ledger_when_non_empty() {
        let store = Arc::new(TodoStore::new());
        let provider = TodoContextProvider::new(store.clone());
        assert!(provider.context().is_none(), "空账本首轮不注入");
        store
            .apply(TodoUpdate {
                goal: Some("做点事".into()),
                ..Default::default()
            })
            .unwrap();
        let text = provider.context().expect("非空账本应注入");
        assert!(text.contains(TASK_STATE_HEADER));
        assert!(text.contains("<goal>做点事</goal>"));
    }

    #[test]
    fn provider_nags_after_threshold_when_empty() {
        let store = Arc::new(TodoStore::new());
        let provider = TodoContextProvider::new(store);
        // 前几轮不注入，超过阈值后开始 nag。
        let mut nagged = false;
        for _ in 0..=TODO_NAG_AFTER_ROUNDS {
            if let Some(text) = provider.context() {
                assert!(text.contains("<reminder>"));
                nagged = true;
                break;
            }
        }
        assert!(nagged, "空账本久未更新应触发 nag");
    }
}
