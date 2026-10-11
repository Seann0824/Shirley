//! `recall` 工具：给主 Agent 一个**主动查记忆**的只读入口（`docs/memory.md` §4.4）。
//!
//! 为什么需要它：记忆平时由 runtime 自动注入（`core.md` + 按 query 检索的相关条目），
//! 但注入是**被动**的——模型无法主动追问"关于 X 我还记着什么"。缺这个入口时，模型会
//! 退而用 `bash` 去翻记忆目录、甚至读源码学格式、最后自己写盘（曾真实发生）。给它一个
//! 正规的只读工具，这个动机就消失了。
//!
//! **只读**：工具不提供任何写入路径；记忆写入只由后台 curator 完成（`docs/memory.md`
//! 决策 3）。返回命中条目的**全文**（含 `id` / 类型 / 状态 / 日期 / 主题），便于模型直接
//! 引用，不必再去读文件。

use std::sync::Arc;

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

use super::format::MemoryError;
use super::provider::{status_label, type_label, MemoryRuntime};

/// 默认返回条数。
const DEFAULT_LIMIT: usize = 5;
/// 单次返回条数上限（防一次把整库倒进上下文）。
const MAX_LIMIT: usize = 20;

/// 主动查记忆工具（只读）。持有与 `ContextProvider` 同一份 [`MemoryRuntime`]。
pub struct RecallTool {
    runtime: Arc<MemoryRuntime>,
    definition: ToolDefinition,
}

impl RecallTool {
    pub fn new(runtime: Arc<MemoryRuntime>) -> Self {
        Self {
            runtime,
            definition: ToolDefinition {
                name: "recall".into(),
                description: "Search your long-term memory for entries relevant to a query. \
                    Your memory is maintained automatically by the runtime and is normally \
                    injected into your context, but use this tool when you need to deliberately \
                    look something up (e.g. \"what do I know about the user's preferences?\" or \
                    \"is there a procedure for X?\"). Returns the matching entries with their \
                    full text. Read-only: you cannot write to memory through this tool."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "What to look up in memory (keywords or a natural-language question)."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of entries to return (default 5, max 20).",
                            "minimum": 1,
                            "maximum": MAX_LIMIT
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
        }
    }
}

impl Tool for RecallTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            let args: RecallArgs = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            let query = args.query.trim();
            if query.is_empty() {
                return Err(ToolError::ArgumentsError(
                    "`query` must not be empty".into(),
                ));
            }
            let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
            let hits = runtime.recall(query, limit).await;
            let rendered = render_hits(query, &hits);
            serde_json::to_value(rendered).map_err(|error| {
                ToolError::ExecutionError(format!("failed to serialize tool result: {error}"))
            })
        })
    }
}

#[derive(serde::Deserialize)]
struct RecallArgs {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

/// 把命中渲染成纯文本（模型可直接读）。空结果给出明确文案而非空串。
fn render_hits(query: &str, hits: &[super::index::Scored]) -> String {
    if hits.is_empty() {
        return format!("No memories matched \"{query}\".");
    }
    let mut out = format!("{} memor{} matched \"{query}\":\n", hits.len(), if hits.len() == 1 { "y" } else { "ies" });
    for hit in hits {
        let entry = &hit.entry;
        out.push_str(&format!(
            "\n## {} [{}] ({}, {})\n",
            entry.id,
            type_label(entry.entry_type),
            entry.timeline_date(),
            status_label(entry.status),
        ));
        out.push_str(&format!("subject: {}\n", entry.subject));
        if let Some(scope) = &entry.scope {
            out.push_str(&format!("scope: {scope}\n"));
        }
        out.push_str("---\n");
        out.push_str(entry.body.trim());
        out.push('\n');
    }
    out
}

/// 便于测试：把 `MemoryError` 转成工具错误（保留分类）。
#[allow(dead_code)]
fn memory_error(error: MemoryError) -> ToolError {
    ToolError::ExecutionError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::format::{Confidence, Domain, Entry, EntryStatus, EntryType};
    use crate::memory::store::MemoryStore;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_recall_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn entry(id: &str, subject: &str, entry_type: EntryType, body: &str) -> Entry {
        Entry {
            id: id.into(),
            entry_type,
            domain: Domain::Personal,
            subject: subject.into(),
            created_at: "2026-05-10".into(),
            valid_from: None,
            supersedes: None,
            status: EntryStatus::Active,
            confidence: Confidence::High,
            scope: None,
            utility: None,
            usage_count: None,
            source: vec!["s.jsonl#turn:1".into()],
            body: body.into(),
        }
    }

    #[tokio::test]
    async fn recall_returns_matching_full_text() {
        let root = tmp("match");
        let store = MemoryStore::new(&root);
        store
            .write_entry(&entry(
                "pref-rust-error",
                "rust-error-handling",
                EntryType::Preference,
                "偏好用 thiserror。",
            ))
            .unwrap();
        store
            .write_entry(&entry("fact-db", "database", EntryType::Fact, "项目用 Postgres。"))
            .unwrap();
        let runtime = Arc::new(MemoryRuntime::new(store));
        let tool = RecallTool::new(runtime);
        let input = serde_json::json!({ "query": "rust error style" });
        let out = tool.invoke(input, ToolContext::new()).await.unwrap();
        let text = out.as_str().unwrap();
        assert!(text.contains("thiserror"), "应含命中正文: {text}");
        assert!(text.contains("[preference]"), "应含类型标签: {text}");
        assert!(!text.contains("Postgres"), "无关条目不出现: {text}");
    }

    #[tokio::test]
    async fn recall_empty_query_is_arguments_error() {
        let root = tmp("empty_q");
        let runtime = Arc::new(MemoryRuntime::new(MemoryStore::new(&root)));
        let tool = RecallTool::new(runtime);
        let input = serde_json::json!({ "query": "   " });
        let err = tool.invoke(input, ToolContext::new()).await.unwrap_err();
        assert!(matches!(err, ToolError::ArgumentsError(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn recall_no_match_is_clear_message() {
        let root = tmp("nomatch");
        let store = MemoryStore::new(&root);
        store
            .write_entry(&entry("fact-db", "database", EntryType::Fact, "项目用 Postgres。"))
            .unwrap();
        let runtime = Arc::new(MemoryRuntime::new(store));
        let tool = RecallTool::new(runtime);
        let input = serde_json::json!({ "query": "quantum chromodynamics" });
        let out = tool.invoke(input, ToolContext::new()).await.unwrap();
        let text = out.as_str().unwrap();
        assert!(text.contains("No memories matched"), "应给明确空结果文案: {text}");
    }

    #[tokio::test]
    async fn recall_limit_caps_results() {
        let root = tmp("limit");
        let store = MemoryStore::new(&root);
        for i in 0..6 {
            store
                .write_entry(&entry(
                    &format!("fact-rust-{i}"),
                    "rust",
                    EntryType::Fact,
                    "Rust 相关事实。",
                ))
                .unwrap();
        }
        let runtime = Arc::new(MemoryRuntime::new(store));
        let tool = RecallTool::new(runtime);
        let input = serde_json::json!({ "query": "rust", "limit": 2 });
        let out = tool.invoke(input, ToolContext::new()).await.unwrap();
        let text = out.as_str().unwrap();
        assert!(text.contains("2 memories matched"), "应受 limit 限制: {text}");
    }
}
