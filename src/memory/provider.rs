//! 每轮请求末尾注入记忆（`docs/memory.md` §4）。
//!
//! 复用 SDK 的 [`ContextProvider`] 接缝，与任务账本同款：注入位置是**本轮请求末尾
//! 的一条 system 消息**——不进 `self.messages`、压缩碰不到、不动前缀缓存。
//!
//! **两级注入**（§4.1）：
//! 1. **常驻**：`core.md` 全量（画像 / 明确偏好 / 活跃项目）——小、稳定，缓存友好；
//! 2. **相关**：按当前 query 从库里检索 top-k 条目的**摘要**（不是全文，全文由
//!    AI 用 `read_file` ��）。
//!
//! **query 从哪来**：`ContextProvider::context(&self)` 没有参数，拿不到"当前这轮
//! 用户说了什么"。所以 provider 持有一个共享运行时 [`MemoryRuntime`]，里面有个
//! `Mutex<Option<String>>` query 槽——驱动方在发起一轮前 [`MemoryRuntime::set_query`]。
//! 该运行时**按 `Agent`（会话）一份**，多会话并发互不干扰。

use std::sync::{Arc, Mutex};

use shirley_agent_sdk::ContextProvider;

use super::format::{EntryStatus, EntryType};
use super::index::search;
use super::store::MemoryStore;

/// 相关注入的条目上限（§4.3）。
pub const MAX_RELEVANT: usize = 3;
/// 注入文本总字符上限（§4.3）——防记忆自己垄断上下文。
pub const MAX_RENDER_CHARS: usize = 2000;

/// 记忆运行时：持有存储 + 当前 query 槽。`Arc` 共享（provider 与驱动方各持一份）。
pub struct MemoryRuntime {
    store: MemoryStore,
    /// 本轮 query（发起一轮前由驱动方设置）。`None` 表示未知 → 只做常驻注入。
    query: Mutex<Option<String>>,
}

impl MemoryRuntime {
    pub fn new(store: MemoryStore) -> Self {
        Self {
            store,
            query: Mutex::new(None),
        }
    }

    /// 设置本轮 query（驱动方在 `run_stream` 前调用）。
    pub fn set_query(&self, query: impl Into<String>) {
        *self.query.lock().expect("memory query lock poisoned") = Some(query.into());
    }

    /// 渲染本轮要注入的记忆文本（空记忆返回 `None`，不注入空内容）。
    ///
    /// 供 [`MemoryContextProvider`] 调用，也可被测试直接调用。
    pub fn render(&self) -> Option<String> {
        let core = self.store.read_core();
        let query = self.query.lock().expect("memory query lock poisoned").clone();
        let relevant = match query.as_deref() {
            Some(q) if !q.trim().is_empty() => {
                let entries = self.store.list_entries().unwrap_or_default();
                let owned: Vec<(String, super::format::Entry)> = entries
                    .into_iter()
                    .map(|(path, entry)| (self.store.relative_path(&path), entry))
                    .collect();
                search(&owned, q, MAX_RELEVANT)
            }
            _ => Vec::new(),
        };

        if core.is_none() && relevant.is_empty() {
            return None;
        }

        let mut body = String::new();
        if let Some(core) = &core {
            body.push_str("<core>\n");
            body.push_str(core.trim());
            body.push_str("\n</core>\n");
        }
        if !relevant.is_empty() {
            body.push_str("<relevant>\n");
            for hit in &relevant {
                body.push_str(&format!(
                    "- [{}] {}（{}，{}）\n",
                    type_label(hit.entry.entry_type),
                    escape(&hit.entry.summary()),
                    hit.entry.timeline_date(),
                    status_label(hit.entry.status),
                ));
            }
            body.push_str("</relevant>\n");
        }

        let mut rendered = format!("<memory_context>\n{body}</memory_context>");
        if rendered.chars().count() > MAX_RENDER_CHARS {
            rendered = rendered.chars().take(MAX_RENDER_CHARS).collect();
            rendered.push_str("\n... (memory context truncated)");
        }
        Some(rendered)
    }
}

/// 每轮请求末尾注入记忆的提供者。
pub struct MemoryContextProvider {
    runtime: Arc<MemoryRuntime>,
}

impl MemoryContextProvider {
    pub fn new(runtime: Arc<MemoryRuntime>) -> Self {
        Self { runtime }
    }
}

impl ContextProvider for MemoryContextProvider {
    fn context(&self) -> Option<String> {
        self.runtime.render()
    }
}

fn type_label(entry_type: EntryType) -> &'static str {
    match entry_type {
        EntryType::Preference => "preference",
        EntryType::Fact => "fact",
        EntryType::Event => "event",
        EntryType::Procedure => "procedure",
    }
}

fn status_label(status: EntryStatus) -> &'static str {
    match status {
        EntryStatus::Active => "active",
        EntryStatus::Superseded => "superseded",
        EntryStatus::Unconfirmed => "unconfirmed",
    }
}

/// XML 文本转义（与 `todo.rs` 同款）。
fn escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::format::{Confidence, Entry};

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_mem_provider_{tag}_{}_{}",
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

    #[test]
    fn empty_memory_renders_none() {
        let root = tmp("empty");
        let runtime = MemoryRuntime::new(MemoryStore::new(&root));
        assert!(runtime.render().is_none());
        assert!(MemoryContextProvider::new(Arc::new(runtime)).context().is_none());
    }

    #[test]
    fn core_is_always_injected_without_query() {
        let root = tmp("core");
        let store = MemoryStore::new(&root);
        store.write_core("用户是 Sean，偏好 Rust。").unwrap();
        let runtime = MemoryRuntime::new(store);
        let rendered = runtime.render().unwrap();
        assert!(rendered.contains("<memory_context>"));
        assert!(rendered.contains("用户是 Sean，偏好 Rust。"));
        assert!(!rendered.contains("<relevant>"));
    }

    #[test]
    fn relevant_injected_from_query() {
        let root = tmp("relevant");
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
        let runtime = MemoryRuntime::new(store);
        runtime.set_query("rust error style");
        let rendered = runtime.render().unwrap();
        assert!(rendered.contains("<relevant>"));
        assert!(rendered.contains("[preference]"));
        assert!(rendered.contains("thiserror"));
        assert!(!rendered.contains("Postgres"), "unrelated entry should not appear");
    }

    #[test]
    fn relevant_respects_top_k() {
        let root = tmp("topk");
        let store = MemoryStore::new(&root);
        for i in 0..6 {
            store
                .write_entry(&entry(
                    &format!("fact-{i}"),
                    "topic",
                    EntryType::Fact,
                    &format!("topic number {i}"),
                ))
                .unwrap();
        }
        let runtime = MemoryRuntime::new(store);
        runtime.set_query("topic");
        let rendered = runtime.render().unwrap();
        assert_eq!(rendered.matches("] topic number").count(), MAX_RELEVANT);
    }

    #[test]
    fn truncates_to_budget() {
        let root = tmp("budget");
        let store = MemoryStore::new(&root);
        store.write_core(&"长".repeat(5000)).unwrap();
        let runtime = MemoryRuntime::new(store);
        let rendered = runtime.render().unwrap();
        assert!(rendered.chars().count() <= MAX_RENDER_CHARS + 40);
        assert!(rendered.contains("truncated"));
    }

    #[test]
    fn provider_uses_runtime_query() {
        let root = tmp("provider");
        let store = MemoryStore::new(&root);
        store
            .write_entry(&entry("fact-rust", "rust", EntryType::Fact, "Rust 相关事实。"))
            .unwrap();
        let runtime = Arc::new(MemoryRuntime::new(store));
        let provider = MemoryContextProvider::new(runtime.clone());
        assert!(provider.context().is_none(), "no query, no core → None");
        runtime.set_query("rust");
        assert!(provider.context().unwrap().contains("Rust 相关事实。"));
    }
}
