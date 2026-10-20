//! Inject memory at the end of each request round (`docs/memory.md` 4).
//!
//! Reuses the SDK's [`ContextProvider`] seam, same as the task ledger: the injection point is **a system message at the end of this round's request**
//! -- it does not enter `self.messages`, compaction cannot touch it, and the prefix cache is untouched.
//!
//! **Two-level injection** (4.1):
//! 1. **Resident**: the full `core.md` (profile / explicit preferences / active projects) -- small, stable, cache-friendly;
//! 2. **Relevant**: the **summaries** of the top-k entries retrieved from the store for the current query (not the full text; the full text is fetched by
//!    the AI with `read`).
//!
//! **Where the query comes from**: `ContextProvider::context(&self)` takes no argument, so it cannot see "what the user said in this round".
//! So the provider holds a shared runtime [`MemoryRuntime`], which contains a
//! `Mutex<Option<String>>` query slot -- the driver sets it via [`MemoryRuntime::set_query`] before starting a round.
//! This runtime is **one per `Agent` (session)**, so concurrent sessions do not interfere.
//!
//! **V2.5 hybrid retrieval**: when embedding is configured, the relevant leg goes through [`hybrid_search`] (BM25 + vectors,
//! RRF fusion); `ContextProvider::context()` is synchronous while embedding is async HTTP, so
//! the query vector is **prefetched** by the driver before starting a round via [`MemoryRuntime::prefetch_query_vector`]
//! and stored in the `query_vector` slot. Without embedding configured -> pure BM25 (honest degradation).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use shirley_agent_sdk::ContextProvider;

use super::embed::{EmbedError, Embedder};
use super::format::{Entry, EntryStatus, EntryType, MemoryError};
use super::index::{Scored, hybrid_search};
use super::store::MemoryStore;
use super::vector::VectorStore;

/// Upper limit of entries for relevant injection (4.3).
pub const MAX_RELEVANT: usize = 3;
/// Total character limit for injected text (4.3) -- prevents memory from monopolizing the context.
pub const MAX_RENDER_CHARS: usize = 2000;

/// Memory runtime: holds the store + the current query slot. Shared via `Arc` (the provider and the driver each hold one).
pub struct MemoryRuntime {
    store: MemoryStore,
    /// This round's query (set by the driver before starting a round). `None` means unknown -> only resident injection.
    query: Mutex<Option<String>>,
    /// **V2.5**: the prefetched query vector for this round (when embedding is configured and prefetch succeeded). `None` =
    /// no vector -> the relevant leg degrades to pure BM25.
    query_vector: Mutex<Option<Vec<f32>>>,
    /// **V2.5**: the embedding client. `None` = not configured -> pure BM25 (honest degradation).
    embedder: Option<Arc<Embedder>>,
    /// **V2-D hit counts**: entry ids hit by retrieval this round -> counts, accumulated in memory and written back in one go
    /// by [`MemoryRuntime::flush_usage`] when curation persists (avoiding a disk write every round).
    usage: Mutex<HashMap<String, u64>>,
}

impl MemoryRuntime {
    /// Pure BM25 runtime (no embedding configured). For tests and the "no embedding" scenario.
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
    pub fn new(store: MemoryStore) -> Self {
        Self {
            store,
            query: Mutex::new(None),
            query_vector: Mutex::new(None),
            embedder: None,
            usage: Mutex::new(HashMap::new()),
        }
    }

    /// Runtime with embedding (V2.5 hybrid retrieval). When `embedder` is `None`, equivalent to [`Self::new`].
    pub fn with_embedder(store: MemoryStore, embedder: Option<Arc<Embedder>>) -> Self {
        Self {
            store,
            query: Mutex::new(None),
            query_vector: Mutex::new(None),
            embedder,
            usage: Mutex::new(HashMap::new()),
        }
    }

    /// Set this round's query (called by the driver before `run_stream`). Also **clears the previous round's query vector**
    /// -- avoiding the silent error of "a new query paired with an old vector"; the new vector is filled by [`Self::prefetch_query_vector`].
    pub fn set_query(&self, query: impl Into<String>) {
        *self.query.lock().expect("memory query lock poisoned") = Some(query.into());
        *self
            .query_vector
            .lock()
            .expect("memory query vector lock poisoned") = None;
    }

    /// **V2.5**: prefetch the embedding for this round's query (the driver `await`s before `run_stream`).
    ///
    /// When embedding is not configured / the query is empty / the call fails, it is `Ok(())` -- the relevant leg falls back to pure BM25,
    /// **never letting an embedding failure interrupt the conversation** (honest degradation: no pretending there is a semantic leg).
    pub async fn prefetch_query_vector(&self) -> Result<(), EmbedError> {
        if let Some(global) = self.store.primary_root().parent()
            && global.join("remote.json").exists()
        {
            match tokio::time::timeout(
                std::time::Duration::from_secs(3),
                crate::remote::sync_once(global),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => eprintln!("[memory:sync] using offline memory: {error}"),
                Err(_) => {
                    eprintln!("[memory:sync] pre-turn sync timed out; using offline memory");
                }
            }
        }
        let Some(embedder) = self.embedder.as_ref() else {
            return Ok(());
        };
        let query = self
            .query
            .lock()
            .expect("memory query lock poisoned")
            .clone();
        let Some(query) = query.filter(|q| !q.trim().is_empty()) else {
            return Ok(());
        };
        let mut vectors = embedder.embed(std::slice::from_ref(&query)).await?;
        let vector = vectors.pop().unwrap_or_default();
        *self
            .query_vector
            .lock()
            .expect("memory query vector lock poisoned") = (!vector.is_empty()).then_some(vector);
        Ok(())
    }

    /// **V2.5**: embed entries in the store that **do not yet have a vector** and persist to the sidecar (called after curation). Also cleans up
    /// residual vectors of deleted entries. A no-op when embedding is not configured. Returns the number of newly embedded entries.
    pub async fn refresh_vectors(&self) -> Result<usize, EmbedError> {
        let Some(embedder) = self.embedder.as_ref() else {
            return Ok(0);
        };
        let mut count = 0;
        for root in self.store.roots() {
            let entries = self
                .store
                .list_entries_in(root)
                .map_err(|error| EmbedError::Transport(error.to_string()))?;
            let vectors = VectorStore::load_merged(std::slice::from_ref(root), embedder.model());
            let missing: Vec<(String, String)> = entries
                .iter()
                .filter(|(_, entry)| vectors.get(&entry.id).is_none())
                .map(|(_, entry)| (entry.id.clone(), embed_text(entry)))
                .collect();
            let new_vectors = if missing.is_empty() {
                Vec::new()
            } else {
                embedder
                    .embed(
                        &missing
                            .iter()
                            .map(|(_, text)| text.clone())
                            .collect::<Vec<_>>(),
                    )
                    .await?
            };
            // Sync may update entries during the embedding request. Re-read under the same
            // OS lock and install vectors only for unchanged text, into the correct domain root.
            let _lock = super::store::lock_root(root)
                .map_err(|error| EmbedError::Transport(error.to_string()))?;
            let current: HashMap<String, Entry> = self
                .store
                .list_entries_in(root)
                .map_err(|error| EmbedError::Transport(error.to_string()))?
                .into_iter()
                .map(|(_, entry)| (entry.id.clone(), entry))
                .collect();
            let mut latest = VectorStore::load_merged(std::slice::from_ref(root), embedder.model());
            let mut changed = latest.retain_ids(current.keys().map(String::as_str)) > 0;
            for ((id, text), vector) in missing.into_iter().zip(new_vectors) {
                if current
                    .get(&id)
                    .is_some_and(|entry| embed_text(entry) == text)
                {
                    latest.upsert(id, vector);
                    count += 1;
                    changed = true;
                }
            }
            if changed {
                latest
                    .save(root)
                    .map_err(|error| EmbedError::Transport(error.to_string()))?;
            }
        }
        Ok(count)
    }

    /// Load the current vector store (multi-root merge); returns an empty store when embedding is not configured.
    fn load_vectors(&self) -> VectorStore {
        match self.embedder.as_ref() {
            Some(embedder) => VectorStore::load_merged(self.store.roots(), embedder.model()),
            None => VectorStore::default(),
        }
    }

    /// Called by the `recall` tool: do **hybrid retrieval** by query, returning the top-`limit` hits (with full text).
    ///
    /// Unlike the automatic injection path (`render`): here we can `await`, so we **embed the query vector on demand**
    /// without relying on the driver to prefetch. When embedding is not configured / embedding fails, it degrades to pure BM25 (honest degradation).
    /// Hits also count toward V2-D's `usage` (shared with automatic injection).
    pub async fn recall(&self, query: &str, limit: usize) -> Vec<Scored> {
        let entries = self.store.list_entries().unwrap_or_default();
        let owned: Vec<(String, Entry)> = entries
            .into_iter()
            .map(|(path, entry)| (self.store.relative_path(&path), entry))
            .collect();
        let vectors = self.load_vectors();
        let query_vector = match self.embedder.as_ref() {
            Some(embedder) => {
                let texts = [query.to_string()];
                embedder
                    .embed(&texts)
                    .await
                    .ok()
                    .and_then(|mut vectors| vectors.pop())
                    .filter(|vector| !vector.is_empty())
            }
            None => None,
        };
        let hits = hybrid_search(&owned, query, query_vector.as_deref(), &vectors, limit);
        if !hits.is_empty() {
            let mut usage = self.usage.lock().expect("memory usage lock poisoned");
            for hit in &hits {
                *usage.entry(hit.entry.id.clone()).or_insert(0) += 1;
            }
        }
        // Debug trace (local stderr only; never enters the model context).
        trace_hits("recall", query, query_vector.is_some(), &hits);
        hits
    }

    /// Render the memory text to inject this round (an empty memory returns `None`; empty content is not injected).
    ///
    /// Called by [`MemoryContextProvider`], and can also be called directly by tests.
    pub fn render(&self) -> Option<String> {
        let core = self.store.read_core();
        let query = self
            .query
            .lock()
            .expect("memory query lock poisoned")
            .clone();
        let relevant = match query.as_deref() {
            Some(q) if !q.trim().is_empty() => {
                let entries = self.store.list_entries().unwrap_or_default();
                let owned: Vec<(String, super::format::Entry)> = entries
                    .into_iter()
                    .map(|(path, entry)| (self.store.relative_path(&path), entry))
                    .collect();
                // V2.5: embedding configured and a prefetched query vector present -> BM25 + vector RRF fusion;
                // otherwise `query_vector` is None -> pure BM25.
                let vectors = self.load_vectors();
                let query_vector = self
                    .query_vector
                    .lock()
                    .expect("memory query vector lock poisoned")
                    .clone();
                hybrid_search(&owned, q, query_vector.as_deref(), &vectors, MAX_RELEVANT)
            }
            _ => Vec::new(),
        };

        // V2-D: record hits (flushed to `usage_count` during curation).
        if !relevant.is_empty() {
            let mut usage = self.usage.lock().expect("memory usage lock poisoned");
            for hit in &relevant {
                *usage.entry(hit.entry.id.clone()).or_insert(0) += 1;
            }
        }
        // Debug trace (local stderr only; never enters the model context).
        if let Some(q) = query.as_deref().filter(|q| !q.trim().is_empty()) {
            trace_hits("inject", q, self.query_vector_present(), &relevant);
        }

        if core.is_none() && relevant.is_empty() {
            return None;
        }

        let mut body = String::new();
        if let Some(core) = &core {
            // Strip `core.md`'s **program metadata header** (the `# Memory Core` title + `> maintained by the program...`
            // comment) before injecting: that is maintenance documentation written for humans and would hint to the model that "an operable file exists",
            // inducing it to use bash to read/write the memory directory (this really happened). Inject only the entry body.
            body.push_str("<core>\n");
            body.push_str(&strip_core_header(core));
            body.push_str("\n</core>\n");
        }
        if !relevant.is_empty() {
            body.push_str("<relevant>\n");
            for hit in &relevant {
                let _ = writeln!(
                    body,
                    "- [{}] {} ({}, {})",
                    type_label(hit.entry.entry_type),
                    escape(&hit.entry.summary()),
                    hit.entry.timeline_date(),
                    status_label(hit.entry.status),
                );
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

impl MemoryRuntime {
    /// **V2-D**: write the accumulated hit counts in memory back to the entries (`usage_count` accumulation), and maintain
    /// `utility` accordingly (derived from hit counts when missing). Called when curation persists.
    ///
    /// Only writes back entries that **still exist** (superseded / deleted ones are skipped); clears the count slot after writing.
    /// Returns the number of entries actually updated.
    pub fn flush_usage(&self) -> Result<usize, MemoryError> {
        let pending: HashMap<String, u64> = {
            let mut usage = self.usage.lock().expect("memory usage lock poisoned");
            if usage.is_empty() {
                return Ok(0);
            }
            std::mem::take(&mut *usage)
        };
        let _locks = self.store.lock_roots()?;
        let mut by_id: HashMap<String, Entry> = self
            .store
            .list_entries()?
            .into_iter()
            .map(|(_, entry)| (entry.id.clone(), entry))
            .collect();

        let mut updated = 0;
        for (id, delta) in pending {
            let Some(entry) = by_id.get_mut(&id) else {
                continue; // the entry is no longer in the store; discard this count
            };
            entry.usage_count = Some(entry.usage_count.unwrap_or(0) + delta);
            if entry.utility.is_none() {
                entry.utility = Some(utility_from_usage(entry.usage_count.unwrap_or(0)));
            }
            self.store.write_entry_unlocked(entry)?;
            updated += 1;
        }
        Ok(updated)
    }
}

/// Derive `utility in [0,1)` from the hit count (saturation curve: `n/(n+5)`).
fn utility_from_usage(usage_count: u64) -> f64 {
    let n = usage_count as f64;
    n / (n + 5.0)
}

/// Provider that injects memory at the end of each request round.
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

/// Text of an entry used for embedding: subject + id + body (`id` carries semantic cues, e.g. `pref-rust-error-style`).
fn embed_text(entry: &Entry) -> String {
    format!("{}\n{}\n{}", entry.subject, entry.id, entry.body)
}

pub(crate) fn type_label(entry_type: EntryType) -> &'static str {
    match entry_type {
        EntryType::Preference => "preference",
        EntryType::Fact => "fact",
        EntryType::Event => "event",
        EntryType::Procedure => "procedure",
    }
}

pub(crate) fn status_label(status: EntryStatus) -> &'static str {
    match status {
        EntryStatus::Active => "active",
        EntryStatus::Superseded => "superseded",
        EntryStatus::Unconfirmed => "unconfirmed",
    }
}

/// Strip `core.md`'s metadata header: the `# Memory Core` title, the immediately following `>` comment lines, and the blank lines
/// between them. Keeps the entry body (`- [type] ...`).
///
/// **May appear multiple times**: with a dual-root store, [`MemoryStore::read_core`] concatenates the global root's and workspace root's
/// respective `core.md`, so there is a second title block; this strips them block by block, leaving only the entry lines.
fn strip_core_header(core: &str) -> String {
    let mut out = Vec::new();
    let mut in_header = false;
    for line in core.lines() {
        let trimmed = line.trim();
        if trimmed == "# Memory Core" {
            in_header = true; // enter a metadata header block (skip its following blank lines and `>` comments)
            continue;
        }
        if in_header {
            if trimmed.is_empty() || trimmed.starts_with('>') {
                continue;
            }
            in_header = false;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

/// XML text escaping (same as `todo.rs`).
fn escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// True when the `SHIRLEY_MEMORY_DEBUG` env var is set to a truthy value.
///
/// **Local debugging only** -- this switch changes nothing about retrieval and nothing about what
/// is injected into the model context; it only gates a stderr trace of which retrieval leg(s)
/// produced each hit (`bm25` / `vector` / `both`).
fn memory_debug_enabled() -> bool {
    matches!(
        std::env::var("SHIRLEY_MEMORY_DEBUG").as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "on")
    )
}

impl MemoryRuntime {
    /// Whether a query vector was prefetched for this round (debug observability).
    fn query_vector_present(&self) -> bool {
        self.query_vector
            .lock()
            .expect("memory query vector lock poisoned")
            .is_some()
    }
}

/// Emit a one-line-per-hit stderr trace of the retrieval leg(s) behind each hit.
///
/// `leg` is `"inject"` (automatic per-round injection via `render`) or `"recall"` (the `recall`
/// tool). `has_vector` says whether the semantic leg actually ran this round (prefetched query
/// vector present); when `false`, every hit is necessarily `bm25`. No-op unless
/// `SHIRLEY_MEMORY_DEBUG` is set.
fn trace_hits(leg: &str, query: &str, has_vector: bool, hits: &[Scored]) {
    if !memory_debug_enabled() {
        return;
    }
    eprintln!(
        "[memory:debug] {leg} query={query:?} semantic_leg={} hits={}",
        if has_vector { "on" } else { "off (bm25 only)" },
        hits.len()
    );
    for (rank, hit) in hits.iter().enumerate() {
        eprintln!(
            "[memory:debug]   #{rank} source={} score={:.4} id={}",
            hit.source.label(),
            hit.score,
            hit.entry.id
        );
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn strip_core_header_drops_metadata_keeps_entries() {
        let raw = "# Memory Core\n\n> Maintained by the program (projection of active entries); do not edit by hand.\n\n- [fact] The user is Sean.\n- [preference] Prefers Rust.\n";
        let stripped = strip_core_header(raw);
        assert!(
            !stripped.contains("# Memory Core"),
            "should strip the title: {stripped}"
        );
        assert!(
            !stripped.contains("do not edit"),
            "should strip the maintenance comment: {stripped}"
        );
        assert!(
            stripped.contains("The user is Sean."),
            "should keep the entry: {stripped}"
        );
        assert!(
            stripped.starts_with("- [fact]"),
            "should start with the first entry: {stripped}"
        );
    }

    #[test]
    fn strip_core_header_handles_empty_and_bodyless() {
        assert_eq!(strip_core_header(""), "");
        // only a header, no entries -> empty string (do not inject an empty core).
        assert_eq!(
            strip_core_header("# Memory Core\n\n> Maintained by the program; do not edit.\n"),
            ""
        );
    }

    #[test]
    fn strip_core_header_drops_every_header_block() {
        // dual-root concatenation: global + workspace each carry a title block; both must be stripped, leaving only entries.
        let raw = "# Memory Core\n\n> Maintained by the program; do not edit.\n\n- [fact] Personal fact.\n\n# Memory Core\n\n> Maintained by the program; do not edit.\n\n- [fact] Project fact.\n";
        let stripped = strip_core_header(raw);
        assert_eq!(
            stripped,
            "- [fact] Personal fact.\n\n- [fact] Project fact."
        );
        assert!(!stripped.contains("# Memory Core"));
        assert!(!stripped.contains("do not edit"));
    }
    use super::*;
    use crate::memory::format::{Confidence, Domain};

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

    #[test]
    fn empty_memory_renders_none() {
        let root = tmp("empty");
        let runtime = MemoryRuntime::new(MemoryStore::new(&root));
        assert!(runtime.render().is_none());
        assert!(
            MemoryContextProvider::new(Arc::new(runtime))
                .context()
                .is_none()
        );
    }

    #[test]
    fn core_is_always_injected_without_query() {
        let root = tmp("core");
        let store = MemoryStore::new(&root);
        store
            .write_core("The user is Sean and prefers Rust.")
            .unwrap();
        let runtime = MemoryRuntime::new(store);
        let rendered = runtime.render().unwrap();
        assert!(rendered.contains("<memory_context>"));
        assert!(rendered.contains("The user is Sean and prefers Rust."));
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
        let runtime = MemoryRuntime::new(store);
        runtime.set_query("rust error style");
        let rendered = runtime.render().unwrap();
        assert!(rendered.contains("<relevant>"));
        assert!(rendered.contains("[preference]"));
        assert!(rendered.contains("thiserror"));
        assert!(
            !rendered.contains("Postgres"),
            "unrelated entry should not appear"
        );
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
        store.write_core(&"é".repeat(5000)).unwrap();
        let runtime = MemoryRuntime::new(store);
        let rendered = runtime.render().unwrap();
        assert!(rendered.chars().count() <= MAX_RENDER_CHARS + 40);
        assert!(rendered.contains("truncated"));
    }

    #[test]
    fn flush_usage_accumulates_and_derives_utility() {
        let root = tmp("flush");
        let store = MemoryStore::new(&root);
        store
            .write_entry(&entry(
                "fact-rust",
                "rust",
                EntryType::Fact,
                "Facts about Rust.",
            ))
            .unwrap();
        let runtime = MemoryRuntime::new(MemoryStore::new(&root));

        runtime.set_query("rust");
        let _ = runtime.render().unwrap();
        let _ = runtime.render().unwrap(); // hit twice
        let updated = runtime.flush_usage().unwrap();
        assert_eq!(updated, 1);

        let entries = store.list_entries().unwrap();
        let saved = entries
            .iter()
            .find(|(_, e)| e.id == "fact-rust")
            .map(|(_, e)| e)
            .unwrap();
        assert_eq!(saved.usage_count, Some(2));
        // utility is derived from the hit count (2/(2+5)), not None.
        assert!(saved.utility.is_some());

        // a second flush with no new hits -> no-op.
        assert_eq!(runtime.flush_usage().unwrap(), 0);
    }

    #[test]
    fn flush_usage_drops_counts_for_missing_entries() {
        let root = tmp("flush_missing");
        let store = MemoryStore::new(&root);
        store
            .write_entry(&entry("fact-gone", "topic", EntryType::Fact, "topic here"))
            .unwrap();
        let runtime = MemoryRuntime::new(MemoryStore::new(&root));
        runtime.set_query("topic");
        let _ = runtime.render().unwrap();
        // after the hit the entry is deleted (simulating supersede / cleanup).
        store.delete_entry("fact-gone").unwrap();
        assert_eq!(runtime.flush_usage().unwrap(), 0);
    }

    #[test]
    fn provider_uses_runtime_query() {
        let root = tmp("provider");
        let store = MemoryStore::new(&root);
        store
            .write_entry(&entry(
                "fact-rust",
                "rust",
                EntryType::Fact,
                "Facts about Rust.",
            ))
            .unwrap();
        let runtime = Arc::new(MemoryRuntime::new(store));
        let provider = MemoryContextProvider::new(Arc::clone(&runtime));
        assert!(provider.context().is_none(), "no query, no core -> None");
        runtime.set_query("rust");
        assert!(provider.context().unwrap().contains("Facts about Rust."));
    }
}
