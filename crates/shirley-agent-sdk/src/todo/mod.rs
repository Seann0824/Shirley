//! 任务账本（todo）：模型自己维护、跨上下文压缩存活的任务状态。
//!
//! 定位：compaction 的配套能力，**SDK 内部持有，应用层无感**——与 [`crate::recall`] 同级。
//!
//! **为什么需要它**：压缩会清空工具输出、把对话压成有损摘要，模型因此丢失
//! "我做到哪了、已经知道什么"，于是重新探索、再压缩，形成正反馈回路
//! （压缩 → 重探索 → 上下文再涨 → 再压缩）。账本把"目标 / 步骤 / 已得结论 /
//! 待决问题"记在**对话之外**：
//!
//! - **模型写**：`todo` 工具暴露给模型，模型自己决定何时更新（不做程序化推断）。
//! - **SDK 注入**：`Agent` 在组装每轮请求时把账本渲染成一条 system 消息
//!   **追加在末尾**——不参与压缩、不破坏 KV 前缀缓存（前面的消息不动）。
//! - **跨压缩存活**：账本不在 `self.messages` 里，压缩碰不到它。
//!
//! 账本不是"再摘要一遍"（那是二次损失）：写进去的是模型自己确认过的事实。

use crate::tool::{Tool, ToolDefinition, ToolError, ToolFuture};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// 账本渲染成注入文本时的字符上限。
///
/// 账本本身也会占上下文，超限截断并显式标注（防它自己垄断上下文，
/// 与 `recall` 的注入截断同一思路）。
const MAX_RENDER_CHARS: usize = 4000;

/// 任务账本的内存存储。
///
/// 与 [`crate::recall::RecallStore`] 同款：`Arc` 共享（runtime 与 `todo` 工具各持一份），
/// 内部可变性走 `Mutex`。持久化留空（进程结束即失，与会话日志无关）。
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

#[derive(Debug, Clone)]
struct Step {
    text: String,
    done: bool,
}

/// 账本更新指令（`todo` 工具的入参）。
///
/// **补丁语义**：只处理显式提供的字段；未提供的字段保持原样。
/// - `goal` / `steps`：整体替换（`steps` 是清单，模型每轮重发全量，天然自纠）；
/// - `add_findings` / `add_open_questions`：追加（"我又发现了 X" 更自然）；
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
    #[serde(default)]
    pub done: bool,
}

impl TodoStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(TaskState::default()),
        }
    }

    /// 清空账本。切换会话时用（`Agent::switch_session`）——旧会话的任务状态
    /// 绝不能残留到新会话。与 `index` / `clear` 一样走内部可变性（`&self`）。
    pub fn clear(&self) {
        let mut state = self.inner.lock().expect("todo lock poisoned");
        *state = TaskState::default();
    }

    /// 应用一次更新，返回更新后的账本渲染文本（空账本返回 `None`）。
    ///
    /// 返回值同时用作 `todo` 工具的调用结果——让模型看到写入后的完整状态，
    /// 确认更新生效、并据此续接。
    pub fn apply(&self, update: TodoUpdate) -> Option<String> {
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
                        done: step.done,
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
        self.render()
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
                let mark = if step.done { "[x]" } else { "[ ]" };
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

/// XML 文本转义：防止模型写入的 `<` / `&` 破坏标签结构。
fn escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `todo` 工具：模型维护自己任务账本的唯一入口。
///
/// 手写实现 `Tool`（与 `RecallTool` 同款）：状态（`Arc<TodoStore>`）由工具自己持有，
/// 与 runtime 共享同一份 `Arc`，构造时绑定比走 `ToolContext` 注入更直接。
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
                description: "Maintain your own task ledger. Record the goal, the step checklist, \
                    findings (facts you have learned: file contents, command results, decisions) and \
                    open questions. The ledger is injected into every request and survives context \
                    compaction, so keeping it current prevents you from repeating work that was already \
                    done. Update it whenever you finish a step or learn something worth keeping."
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
                            "description": "Replace the full step checklist.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string" },
                                    "done": { "type": "boolean", "default": false }
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

    fn invoke(&self, input: serde_json::Value, _ctx: crate::tool::ToolContext) -> ToolFuture<'_> {
        let store = self.store.clone();
        Box::pin(async move {
            let update: TodoUpdate = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            let rendered = store
                .apply(update)
                .unwrap_or_else(|| "(task ledger is empty)".to_string());
            serde_json::to_value(format!("task ledger updated:\n{rendered}")).map_err(|error| {
                ToolError::ExecutionError(format!("failed to serialize tool result: {error}"))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_then_render_roundtrip() {
        let store = TodoStore::new();
        assert!(store.render().is_none(), "空账本不渲染");

        let out = store.apply(TodoUpdate {
            goal: Some("实现 todo 工具".into()),
            steps: Some(vec![
                TodoStep { text: "建模块".into(), done: true },
                TodoStep { text: "接运行时".into(), done: false },
            ]),
            add_findings: Some(vec!["压缩会清空工具输出".into()]),
            add_open_questions: None,
            clear: None,
        });
        let text = out.expect("非空账本应渲染");
        assert!(text.contains("<goal>实现 todo 工具</goal>"));
        assert!(text.contains("- [x] 建模块"));
        assert!(text.contains("- [ ] 接运行时"));
        assert!(text.contains("<findings>"));
        assert!(text.contains("压缩会清空工具输出"));
    }

    #[test]
    fn patch_only_touches_provided_fields() {
        let store = TodoStore::new();
        store.apply(TodoUpdate {
            goal: Some("g".into()),
            steps: Some(vec![TodoStep { text: "s1".into(), done: false }]),
            ..Default::default()
        });
        // 只追加一条 finding，goal 与 steps 不受影响。
        store.apply(TodoUpdate {
            add_findings: Some(vec!["f1".into()]),
            ..Default::default()
        });
        let text = store.render().unwrap();
        assert!(text.contains("<goal>g</goal>"));
        assert!(text.contains("- [ ] s1"));
        assert!(text.contains("f1"));
    }

    #[test]
    fn clear_resets_everything() {
        let store = TodoStore::new();
        store.apply(TodoUpdate {
            goal: Some("g".into()),
            ..Default::default()
        });
        assert!(store.render().is_some());
        store.apply(TodoUpdate { clear: Some(true), ..Default::default() });
        assert!(store.render().is_none());
    }

    #[test]
    fn escape_protects_tag_structure() {
        let store = TodoStore::new();
        store.apply(TodoUpdate {
            add_findings: Some(vec!["a < b & c > d".into()]),
            ..Default::default()
        });
        let text = store.render().unwrap();
        assert!(text.contains("a &lt; b &amp; c &gt; d"));
    }

    #[tokio::test]
    async fn todo_tool_returns_updated_ledger() {
        let store = Arc::new(TodoStore::new());
        let tool = TodoTool::new(store);
        let output = tool
            .invoke(
                serde_json::json!({ "goal": "目标", "steps": [{ "text": "步骤", "done": true }] }),
                crate::tool::ToolContext::new(),
            )
            .await
            .expect("调用应成功");
        let text = output.as_str().expect("返回应是字符串");
        assert!(text.contains("task ledger updated"));
        assert!(text.contains("<goal>目标</goal>"));
        assert!(text.contains("- [x] 步骤"));
    }

    #[tokio::test]
    async fn todo_tool_rejects_unknown_field() {
        let tool = TodoTool::new(Arc::new(TodoStore::new()));
        let err = tool
            .invoke(serde_json::json!({ "nope": 1 }), crate::tool::ToolContext::new())
            .await;
        assert!(err.is_err());
    }
}
