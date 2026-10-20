//! The `recall` tool: gives the main Agent a read-only entry point to **actively query memory**
//! (`docs/memory.md` 4.4).
//!
//! Memory is normally injected automatically by [`MemoryContextProvider`](crate::memory::MemoryContextProvider)
//! (`core.md` + entries relevant to a query), but that injection is **passive** -- the model
//! cannot actively ask "what else do I remember about X". Without this entry point the model
//! would fall back to `bash` to rummage through the memory directory, even read the source to
//! learn the format, and finally write to disk itself (this really happened). A proper read-only
//! tool removes that motivation.
//!
//! **Read-only**: no write path whatsoever; memory writes are done only by the background curator
//! (`docs/memory.md` decision 3). Returns the **full text** of matching entries (with
//! `id` / type / status / date / subject), so the model can cite them without re-reading files.

use std::fmt::Write as _;
use std::sync::Arc;

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

use crate::memory::index::Scored;
use crate::memory::provider::{MemoryRuntime, status_label, type_label};

/// Default number of results.
const DEFAULT_LIMIT: usize = 5;
/// Maximum number of results per call (prevents dumping the whole store into the context at once).
const MAX_LIMIT: usize = 20;

/// Active memory-query tool (read-only). Holds the same [`MemoryRuntime`] as the `ContextProvider`.
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
        let runtime = Arc::clone(&self.runtime);
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

/// Render the hits as plain text (the model can read it directly). An empty result gives an explicit message rather than an empty string.
fn render_hits(query: &str, hits: &[Scored]) -> String {
    if hits.is_empty() {
        return format!("No memories matched \"{query}\".");
    }
    let mut out = format!(
        "{} memor{} matched \"{query}\":\n",
        hits.len(),
        if hits.len() == 1 { "y" } else { "ies" }
    );
    for hit in hits {
        let entry = &hit.entry;
        let _ = writeln!(
            out,
            "\n## {} [{}] ({}, {})",
            entry.id,
            type_label(entry.entry_type),
            entry.timeline_date(),
            status_label(entry.status),
        );
        let _ = writeln!(out, "subject: {}", entry.subject);
        if let Some(scope) = &entry.scope {
            let _ = writeln!(out, "scope: {scope}");
        }
        out.push_str("---\n");
        out.push_str(entry.body.trim());
        out.push('\n');
    }
    out
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
                "Prefers thiserror.",
            ))
            .unwrap();
        store
            .write_entry(&entry(
                "fact-db",
                "database",
                EntryType::Fact,
                "The project uses Postgres.",
            ))
            .unwrap();
        let runtime = Arc::new(MemoryRuntime::new(store));
        let tool = RecallTool::new(runtime);
        let input = serde_json::json!({ "query": "rust error style" });
        let out = tool.invoke(input, ToolContext::new()).await.unwrap();
        let text = out.as_str().unwrap();
        assert!(
            text.contains("thiserror"),
            "should contain the matching body: {text}"
        );
        assert!(
            text.contains("[preference]"),
            "should contain the type label: {text}"
        );
        assert!(
            !text.contains("Postgres"),
            "unrelated entries should not appear: {text}"
        );
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
            .write_entry(&entry(
                "fact-db",
                "database",
                EntryType::Fact,
                "The project uses Postgres.",
            ))
            .unwrap();
        let runtime = Arc::new(MemoryRuntime::new(store));
        let tool = RecallTool::new(runtime);
        let input = serde_json::json!({ "query": "quantum chromodynamics" });
        let out = tool.invoke(input, ToolContext::new()).await.unwrap();
        let text = out.as_str().unwrap();
        assert!(
            text.contains("No memories matched"),
            "should give an explicit empty-result message: {text}"
        );
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
                    "Facts about Rust.",
                ))
                .unwrap();
        }
        let runtime = Arc::new(MemoryRuntime::new(store));
        let tool = RecallTool::new(runtime);
        let input = serde_json::json!({ "query": "rust", "limit": 2 });
        let out = tool.invoke(input, ToolContext::new()).await.unwrap();
        let text = out.as_str().unwrap();
        assert!(
            text.contains("2 memories matched"),
            "should be capped by limit: {text}"
        );
    }
}
