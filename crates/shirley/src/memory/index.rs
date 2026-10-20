//! Index-page maintenance + lightweight keyword retrieval (`docs/memory.md` 3.3 / 4.1).
//!
//! Two things:
//!
//! 1. **`index.md`**: a program-maintained entry page, one line per memory (id / type / summary / path / date).
//!    Chapter 3 warns that "a flat plain-text listing degenerates into islands" -- the index page is the entry point. [`write_index`]
//!    regenerates it from all current entries (idempotent, no incremental update).
//! 2. **Retrieval**: V2 uses BM25 (IDF + term-frequency saturation + document-length normalization); V2.5 adds a **semantic vector leg**
//!    (cosine), and the two legs are fused into [`hybrid_search`] with RRF. **When no embedding is configured it degrades automatically to
//!    pure BM25** (`docs/memory.md` 10).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use super::format::{Entry, IndexLine, MemoryError};
use super::store::{INDEX_FILE, MemoryStore};
use super::vector::{VectorStore, cosine};

/// The score and matching terms of one entry during retrieval.
#[derive(Debug, Clone)]
pub struct Scored {
    pub entry: Entry,
    /// Relative path (for `read` to fetch the full text; not yet wired in V1, retrieval results only use `entry`).
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
    pub path: String,
    /// Match score (higher = more relevant).
    pub score: f64,
    /// **Observability only**: which retrieval leg(s) produced this hit.
    ///
    /// Set by the three legs ([`search`] / [`search_vector`] / [`hybrid_search`]) and used by the local
    /// `SHIRLEY_MEMORY_DEBUG` trace in the provider. It **never enters the model context** -- the injected
    /// text (see `provider::render`) does not read this field.
    pub source: MatchSource,
}

/// Which retrieval leg(s) produced a hit. A tiny bitflag so RRF fusion can union the legs.
///
/// **Debug/observability only** -- the value is never serialized into the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchSource(u8);

impl MatchSource {
    /// Produced by the BM25 (keyword) leg.
    pub const BM25: MatchSource = MatchSource(1);
    /// Produced by the semantic (vector/cosine) leg.
    pub const VECTOR: MatchSource = MatchSource(2);

    /// Union of two sources (used when RRF fuses the two legs by id).
    pub fn union(self, other: MatchSource) -> MatchSource {
        MatchSource(self.0 | other.0)
    }

    pub fn has_bm25(self) -> bool {
        self.0 & 1 != 0
    }

    pub fn has_vector(self) -> bool {
        self.0 & 2 != 0
    }

    /// `bm25` / `vector` / `both` / `none` -- for the debug trace.
    pub fn label(self) -> &'static str {
        match (self.has_bm25(), self.has_vector()) {
            (true, true) => "both",
            (true, false) => "bm25",
            (false, true) => "vector",
            (false, false) => "none",
        }
    }
}

/// Project an index line from one entry.
pub fn index_line(path: &str, entry: &Entry) -> IndexLine {
    IndexLine {
        id: entry.id.clone(),
        entry_type: entry.entry_type,
        path: path.to_string(),
        summary: entry.summary(),
        date: entry.timeline_date().to_string(),
    }
}

/// Rewrite all entries under the given root into that root's `index.md`. An empty store writes a titled empty index page.
///
/// **Written per root**: `entries` should be entries under `root` (paths relative to `root`), and only `root` is written --
/// global (personal) and workspace (project) each write their own index, without bleeding into each other.
pub fn write_index(root: &Path, entries: &[(String, Entry)]) -> Result<(), MemoryError> {
    MemoryStore::ensure_layout_at(root)?;
    let mut lines: Vec<IndexLine> = entries
        .iter()
        .map(|(path, entry)| index_line(path, entry))
        .collect();
    // stable order: reverse chronological (newest first), then by id for the same date.
    lines.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.id.cmp(&b.id)));

    let mut text = String::from("# Memory Index\n\n");
    text.push_str("> Maintained by the program; one line per memory. Regenerated after entry changes; do not edit by hand.\n\n");
    if lines.is_empty() {
        text.push_str("_(no memories yet)_\n");
    } else {
        for line in &lines {
            let _ = writeln!(
                text,
                "- [{}]({}) · {} · {} · {}",
                line.id, line.path, line.entry_type, line.summary, line.date
            );
        }
    }
    super::store::atomic_write(&root.join(INDEX_FILE), text.as_bytes())?;
    Ok(())
}

/// Retrieval: score every entry, return entries with score > 0, sorted by descending score (ties by newest -> oldest).
///
/// **Scoring uses BM25** (`docs/memory.md` 10 V2): term-frequency saturation + document-length normalization + IDF --
/// rare terms weigh more than common ones, and long documents do not win just by piling up words; more stable than V1's fixed-weight substring scoring.
/// Field differences are preserved as **weighted term frequencies** (`subject` highest, body next, `id` / `scope` lowest),
/// so the V1 semantic "subject hit beats body hit" still holds under BM25.
///
/// Parameters (classic BM25): `k1 = 1.2` (term-frequency saturation rate), `b = 0.75` (length-normalization strength).
/// The retrieval interface is unchanged (`query -> relevant entries`); the upgrade does not touch the architecture.
///
/// **V2-D**: the match score is multiplied by a gentle `utility` factor (`0.9 ~ 1.1`) -- entries with a high historical usefulness
/// move slightly ahead at equal relevance, but do **not overwhelm** BM25 relevance. A missing `utility` counts as `1.0`.
pub fn search(entries: &[(String, Entry)], query: &str, limit: usize) -> Vec<Scored> {
    let terms = tokenize(query);
    if terms.is_empty() {
        return Vec::new();
    }

    // weighted term frequency and length of each document.
    let docs: Vec<(HashMap<String, f64>, f64)> =
        entries.iter().map(|(_, entry)| doc_terms(entry)).collect();
    let n = docs.len() as f64;
    let avgdl = if docs.is_empty() {
        0.0
    } else {
        docs.iter().map(|(_, dl)| *dl).sum::<f64>() / n
    };

    // document frequency (number of documents containing the term) -> IDF.
    let mut df: HashMap<&str, usize> = HashMap::new();
    for (tf, _) in &docs {
        for term in tf.keys() {
            *df.entry(term.as_str()).or_insert(0) += 1;
        }
    }
    let idf = |term: &str| -> f64 {
        let df = *df.get(term).unwrap_or(&0) as f64;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    };

    let mut scored: Vec<Scored> = entries
        .iter()
        .zip(docs.iter())
        .filter_map(|((path, entry), (tf, dl))| {
            let score = bm25(tf, *dl, avgdl, &terms, &idf)
                * status_factor(entry.status)
                * utility_factor(entry.utility);
            (score > 0.0).then(|| Scored {
                entry: entry.clone(),
                path: path.clone(),
                score,
                source: MatchSource::BM25,
            })
        })
        .collect();

    // descending score; ties by newest -> oldest (`docs/memory.md` acceptance 3), then stable by id.
    sort_scored(&mut scored);
    scored.truncate(limit);
    scored
}

/// Semantic retrieval leg: score entries by **cosine similarity** (needs a prefetched query vector + stored entry vectors).
///
/// Only entries that **have a vector in `vectors`** are scored (entries never embedded cannot be recalled by this leg -- this is
/// precisely the point of hybrid retrieval: the BM25 leg still covers them). Status / utility factors match the BM25 leg.
/// Returns entries with similarity > 0, sorted by descending similarity (ties by newest -> oldest, then stable by id).
pub fn search_vector(
    entries: &[(String, Entry)],
    query_vector: &[f32],
    vectors: &VectorStore,
    limit: usize,
) -> Vec<Scored> {
    let mut scored: Vec<Scored> = entries
        .iter()
        .filter_map(|(path, entry)| {
            let vector = vectors.get(&entry.id)?;
            let score = cosine(query_vector, vector) as f64
                * status_factor(entry.status)
                * utility_factor(entry.utility);
            (score > 0.0).then(|| Scored {
                entry: entry.clone(),
                path: path.clone(),
                score,
                source: MatchSource::VECTOR,
            })
        })
        .collect();
    sort_scored(&mut scored);
    scored.truncate(limit);
    scored
}

/// RRF constant (Reciprocal Rank Fusion): `k = 60` is the classic value.
const RRF_K: f64 = 60.0;
/// Minimum recall count per leg (recall a bit more before fusion, giving the other leg a chance to fill in).
const LEG_LIMIT_MIN: usize = 20;

/// **Hybrid retrieval**: keyword BM25 leg + semantic vector leg, fused with **RRF (Reciprocal Rank Fusion)**.
///
/// Each leg recalls `max(limit*4, 20)` entries, accumulating scores by `1/(k + rank)` (`k = 60`).
/// RRF looks only at **rank**, not raw scores, naturally avoiding the trouble of "how to normalize BM25 scores against cosine scores"
/// -- which is exactly why it became the standard approach for hybrid retrieval.
///
/// When `query_vector` is `None` (no embedding configured / prefetch failed), it **degrades to pure BM25**,
/// consistent with pre-upgrade behavior (honest degradation, no pretending there is a semantic leg).
pub fn hybrid_search(
    entries: &[(String, Entry)],
    query: &str,
    query_vector: Option<&[f32]>,
    vectors: &VectorStore,
    limit: usize,
) -> Vec<Scored> {
    let leg_limit = (limit * 4).max(LEG_LIMIT_MIN);
    let bm25_hits = search(entries, query, leg_limit);
    let vector_hits = match query_vector {
        Some(query_vector) => search_vector(entries, query_vector, vectors, leg_limit),
        None => Vec::new(),
    };
    // the semantic leg has nothing (not configured / no vectors) -> pure BM25.
    if vector_hits.is_empty() {
        return bm25_hits.into_iter().take(limit).collect();
    }

    // fuse the RRF scores of the two legs by id.
    let mut fused: HashMap<String, Scored> = HashMap::new();
    for hits in [&bm25_hits, &vector_hits] {
        for (rank, hit) in hits.iter().enumerate() {
            let contribution = 1.0 / (RRF_K + (rank + 1) as f64);
            fused
                .entry(hit.entry.id.clone())
                .and_modify(|existing| {
                    existing.score += contribution;
                    // keep track of which legs contributed (debug/observability).
                    existing.source = existing.source.union(hit.source);
                })
                .or_insert_with(|| Scored {
                    entry: hit.entry.clone(),
                    path: hit.path.clone(),
                    score: contribution,
                    source: hit.source,
                });
        }
    }
    let mut scored: Vec<Scored> = fused.into_values().collect();
    sort_scored(&mut scored);
    scored.truncate(limit);
    scored
}

/// Unified ordering: descending score, ties by newest -> oldest, then stable by id.
fn sort_scored(scored: &mut [Scored]) {
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.entry.timeline_date().cmp(a.entry.timeline_date()))
            .then_with(|| a.entry.id.cmp(&b.entry.id))
    });
}

/// Field weights (relative to the body): subject matters most, `id` / `scope` are auxiliary.
const SUBJECT_WEIGHT: f64 = 3.0;
const ID_SCOPE_WEIGHT: f64 = 0.5;

/// Tokenize one entry into a **weighted term-frequency** table + document length (sum of weights).
fn doc_terms(entry: &Entry) -> (HashMap<String, f64>, f64) {
    let mut tf: HashMap<String, f64> = HashMap::new();
    for term in tokenize_counts(&entry.subject) {
        *tf.entry(term).or_insert(0.0) += SUBJECT_WEIGHT;
    }
    for term in tokenize_counts(&entry.body) {
        *tf.entry(term).or_insert(0.0) += 1.0;
    }
    for term in tokenize_counts(&entry.id) {
        *tf.entry(term).or_insert(0.0) += ID_SCOPE_WEIGHT;
    }
    if let Some(scope) = &entry.scope {
        for term in tokenize_counts(scope) {
            *tf.entry(term).or_insert(0.0) += ID_SCOPE_WEIGHT;
        }
    }
    let len = tf.values().sum();
    (tf, len)
}

/// BM25 scoring (sums over unique query terms).
fn bm25(
    tf: &HashMap<String, f64>,
    dl: f64,
    avgdl: f64,
    terms: &HashSet<String>,
    idf: &impl Fn(&str) -> f64,
) -> f64 {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let avgdl = avgdl.max(1.0);
    let mut score = 0.0;
    for term in terms {
        let Some(&f) = tf.get(term) else { continue };
        let denom = f + K1 * (1.0 - B + B * dl / avgdl);
        score += idf(term) * (f * (K1 + 1.0)) / denom;
    }
    score
}

/// Status factor: superseded old entries are downranked (still retrievable, but not prioritized).
fn status_factor(status: super::format::EntryStatus) -> f64 {
    match status {
        super::format::EntryStatus::Superseded => 0.5,
        _ => 1.0,
    }
}

/// Utility factor (V2-D): maps `utility in [0,1]` to a gentle multiplier in `0.9 ~ 1.1`; missing counts as `1.0`.
///
/// Deliberately **only a gentle adjustment** -- relevance (BM25) is still the primary order, and `utility` only fine-tunes when relevance is close,
/// not changing the V1 acceptance criterion of "keyword retrieval + time tiebreak".
fn utility_factor(utility: Option<f64>) -> f64 {
    match utility {
        Some(u) => 0.9 + 0.2 * u.clamp(0.0, 1.0),
        None => 1.0,
    }
}

/// English stop words (no stop-word filtering for CJK -- bigrams are already short enough).
const STOP: [&str; 16] = [
    "the", "a", "an", "of", "to", "and", "or", "is", "are", "in", "on", "for", "with", "that",
    "this", "it",
];

/// Query tokenization: Latin split on non-alphanumeric, stop words removed, lowercased, **deduplicated**; CJK goes through bigrams (see below).
fn tokenize(text: &str) -> HashSet<String> {
    tokenize_counts(text).into_iter().collect()
}

/// Tokenization (**keeps repeat counts**, for BM25 term frequency).
///
/// Two legs:
/// - **Latin / digits**: a run of consecutive `is_alphanumeric` chars is one term; stop words removed, lowercased, length < 2 dropped;
/// - **CJK**: a run of consecutive CJK is split into **bigrams** (two adjacent chars per term; a single-char run keeps that char).
///   This fixes a real defect in V2: previously a whole run of consecutive CJK was treated as **one** term, so
///   "what is my name" and the entry "the user's name is Sean" could never match (a natural-language Chinese query
///   almost always got 0 hits). The bigram is the zero-dependency, dictionary-free fallback for Chinese retrieval,
///   and is naturally compatible with BM25's field-weighted term frequencies.
fn tokenize_counts(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut run = String::new();
    let mut run_is_cjk = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            let is_cjk = is_cjk_char(ch);
            // category switch (Latin <-> CJK): flush the previous run first.
            if !run.is_empty() && is_cjk != run_is_cjk {
                push_run(&mut terms, &run, run_is_cjk);
                run.clear();
            }
            run_is_cjk = is_cjk;
            run.extend(ch.to_lowercase());
        } else if !run.is_empty() {
            push_run(&mut terms, &run, run_is_cjk);
            run.clear();
        }
    }
    if !run.is_empty() {
        push_run(&mut terms, &run, run_is_cjk);
    }
    terms
}

/// A run of same-category chars (Latin or CJK) -> terms.
fn push_run(terms: &mut Vec<String>, run: &str, is_cjk: bool) {
    if is_cjk {
        push_cjk_bigrams(run, terms);
    } else if run.chars().count() >= 2 && !STOP.contains(&run) {
        terms.push(run.to_string());
    }
}

/// Split a CJK run into bigrams: two adjacent chars per term; a single-char run keeps that char.
fn push_cjk_bigrams(run: &str, terms: &mut Vec<String>) {
    let chars: Vec<char> = run.chars().collect();
    if chars.len() == 1 {
        terms.push(chars[0].to_string());
        return;
    }
    for pair in chars.windows(2) {
        terms.push(pair.iter().collect());
    }
}

/// Whether a char is CJK (including extension / compatibility blocks / Japanese kana) -- a rough check by Unicode code point, good enough.
fn is_cjk_char(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF      // Hiragana / Katakana
        | 0x3400..=0x4DBF    // CJK Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xF900..=0xFAFF    // CJK Compatibility
        | 0x20000..=0x2FA1F  // CJK Extensions B~F
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::format::{Confidence, Domain, EntryStatus, EntryType};

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

    fn sample() -> Vec<(String, Entry)> {
        vec![
            (
                "preferences/rust-error-style.md".into(),
                entry(
                    "pref-rust-error-style",
                    "rust-error-handling",
                    EntryType::Preference,
                    "The user prefers thiserror over anyhow for defining domain errors.",
                ),
            ),
            (
                "facts/db.md".into(),
                entry(
                    "fact-db",
                    "database",
                    EntryType::Fact,
                    "The project uses Postgres.",
                ),
            ),
            (
                "episodic/2026-05-12-ts.md".into(),
                entry(
                    "event-ts-migration",
                    "ts-migration",
                    EntryType::Event,
                    "2026-05-12 TS backend migration started.",
                ),
            ),
        ]
    }

    #[test]
    fn search_hits_subject_and_body() {
        let entries = sample();
        let hits = search(&entries, "rust error style", 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.id, "pref-rust-error-style");
    }

    #[test]
    fn search_ranks_subject_higher_than_body() {
        let entries = vec![
            (
                "a.md".into(),
                entry("a", "rust", EntryType::Fact, "unrelated body about rust"),
            ),
            (
                "b.md".into(),
                entry("b", "other", EntryType::Fact, "mentions rust once"),
            ),
        ];
        let hits = search(&entries, "rust", 5);
        assert_eq!(hits[0].entry.id, "a", "subject hit should outrank body hit");
    }

    #[test]
    fn search_empty_query_returns_nothing() {
        assert!(search(&sample(), "   ", 3).is_empty());
    }

    #[test]
    fn utility_factor_is_bounded_and_centered() {
        assert!((utility_factor(None) - 1.0).abs() < 1e-9);
        assert!((utility_factor(Some(0.0)) - 0.9).abs() < 1e-9);
        assert!((utility_factor(Some(1.0)) - 1.1).abs() < 1e-9);
        // out-of-range values are clamped, not amplified into an anomalous multiplier.
        assert!((utility_factor(Some(5.0)) - 1.1).abs() < 1e-9);
        assert!((utility_factor(Some(-3.0)) - 0.9).abs() < 1e-9);
    }

    #[test]
    fn utility_only_breaks_near_ties() {
        // two entries with equal relevance: the one with higher utility moves slightly ahead; but when the relevance gap is large enough, utility cannot flip it.
        let mut a = entry("a", "rust", EntryType::Fact, "rust");
        let mut b = entry("b", "rust", EntryType::Fact, "rust");
        a.utility = Some(1.0);
        b.utility = Some(0.0);
        let hits = search(&[("a.md".into(), a), ("b.md".into(), b)], "rust", 5);
        assert_eq!(hits[0].entry.id, "a");
    }

    #[test]
    fn search_no_match_returns_empty() {
        assert!(search(&sample(), "kubernetes", 3).is_empty());
    }

    #[test]
    fn search_ties_break_by_recency() {
        let entries = vec![
            ("old.md".into(), {
                let mut e = entry("old", "topic", EntryType::Fact, "about topic");
                e.valid_from = Some("2026-01-01".into());
                e
            }),
            ("new.md".into(), {
                let mut e = entry("new", "topic", EntryType::Fact, "about topic");
                e.valid_from = Some("2026-06-01".into());
                e
            }),
        ];
        let hits = search(&entries, "topic", 5);
        assert_eq!(hits[0].entry.id, "new");
        assert_eq!(hits[1].entry.id, "old");
    }

    #[test]
    fn superseded_entry_is_downranked() {
        let entries = vec![
            (
                "active.md".into(),
                entry("active", "topic", EntryType::Preference, "topic active"),
            ),
            ("old.md".into(), {
                let mut e = entry("old", "topic", EntryType::Preference, "topic old");
                e.status = EntryStatus::Superseded;
                e
            }),
        ];
        let hits = search(&entries, "topic", 5);
        assert_eq!(hits[0].entry.id, "active");
    }

    #[test]
    fn cjk_query_matches() {
        let entries = vec![(
            "p.md".into(),
            entry("p", "错误处理", EntryType::Preference, "偏好用 thiserror。"),
        )];
        let hits = search(&entries, "错误处理", 5);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn vector_leg_ranks_by_cosine() {
        let entries = vec![
            ("a.md".into(), entry("a", "s", EntryType::Fact, "alpha")),
            ("b.md".into(), entry("b", "s", EntryType::Fact, "beta")),
        ];
        let mut vectors = VectorStore::new("m");
        vectors.upsert("a", vec![1.0, 0.0]);
        vectors.upsert("b", vec![0.0, 1.0]);
        // query is in the same direction as a -> a should rank first.
        let hits = search_vector(&entries, &[1.0, 0.0], &vectors, 5);
        assert_eq!(hits[0].entry.id, "a");
        // orthogonal to both -> no hits.
        assert!(search_vector(&entries, &[0.0, 0.0], &vectors, 5).is_empty());
    }

    #[test]
    fn hybrid_fuses_both_legs_via_rrf() {
        // a is hit only by BM25 (keywords), b only by the vector leg (semantics) -- after fusion both should be present.
        let entries = vec![
            (
                "a.md".into(),
                entry("a", "note", EntryType::Fact, "thiserror"),
            ),
            (
                "b.md".into(),
                entry("b", "note", EntryType::Fact, "unrelated body"),
            ),
        ];
        let mut vectors = VectorStore::new("m");
        vectors.upsert("a", vec![0.0, 1.0]);
        vectors.upsert("b", vec![1.0, 0.0]);
        let hits = hybrid_search(&entries, "thiserror", Some(&[1.0, 0.0]), &vectors, 5);
        let ids: Vec<&str> = hits.iter().map(|h| h.entry.id.as_str()).collect();
        assert!(
            ids.contains(&"a"),
            "the BM25 leg should contribute a: {ids:?}"
        );
        assert!(
            ids.contains(&"b"),
            "the vector leg should contribute b: {ids:?}"
        );
    }

    #[test]
    fn hybrid_without_query_vector_degrades_to_bm25() {
        let entries = vec![(
            "a.md".into(),
            entry("a", "note", EntryType::Fact, "thiserror"),
        )];
        let vectors = VectorStore::new("m");
        // no query vector -> pure BM25 (result matches search).
        let hybrid = hybrid_search(&entries, "thiserror", None, &vectors, 5);
        let bm25 = search(&entries, "thiserror", 5);
        assert_eq!(hybrid.len(), bm25.len());
        assert_eq!(hybrid[0].entry.id, bm25[0].entry.id);
    }

    #[test]
    fn cjk_natural_language_query_matches_via_bigram() {
        // regression: previously a whole run of consecutive CJK was treated as **one** term, so natural-language questions (differing in wording from the entry)
        // never got hits. After bigrams, "what is my name" should match "the user's name is Sean." (sharing
        // the overlapping character bigrams of the two strings).
        let entries = vec![(
            "n.md".into(),
            entry("n", "user-identity", EntryType::Fact, "用户的名字是 Sean。"),
        )];
        let hits = search(&entries, "我的名字是什么", 5);
        assert_eq!(
            hits.len(),
            1,
            "a natural-language Chinese query should match (bigram)"
        );
        assert_eq!(hits[0].entry.id, "n");
    }

    #[test]
    fn tokenize_splits_cjk_into_bigrams_and_latin_words() {
        let terms = tokenize("用 Rust 写 thiserror 错误");
        assert!(terms.contains("rust"), "Latin words are kept whole");
        assert!(terms.contains("thiserror"));
        assert!(terms.contains("错误"), "a two-char CJK run = one bigram");
        assert!(
            !terms.contains("thiserror错误"),
            "Latin and CJK are different runs and should not be joined into one term"
        );
    }

    #[test]
    fn bm25_rare_term_outweighs_common() {
        // rust appears in both docs, thiserror in only one: when the query contains both, the doc with the rarer term should rank first.
        let entries = vec![
            (
                "a.md".into(),
                entry("a", "rust", EntryType::Fact, "thiserror"),
            ),
            ("b.md".into(), entry("b", "rust", EntryType::Fact, "anyhow")),
        ];
        let hits = search(&entries, "rust thiserror", 5);
        assert_eq!(
            hits[0].entry.id, "a",
            "rare term should outweigh common term"
        );
    }

    #[test]
    fn bm25_length_normalization_penalizes_long_docs() {
        // hitting the same number of times, the short doc should rank before the long one (length normalization).
        let entries = vec![
            (
                "short.md".into(),
                entry("short", "note", EntryType::Fact, "topic"),
            ),
            (
                "long.md".into(),
                entry(
                    "long",
                    "note",
                    EntryType::Fact,
                    "topic alpha beta gamma delta epsilon zeta eta theta",
                ),
            ),
        ];
        let hits = search(&entries, "topic", 5);
        assert_eq!(hits[0].entry.id, "short", "longer doc should be penalized");
    }

    #[test]
    fn write_index_produces_lines() {
        let root = std::env::temp_dir().join(format!(
            "shirley_mem_index_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let entries: Vec<(String, Entry)> = sample();
        write_index(&root, &entries).unwrap();
        let text = std::fs::read_to_string(root.join(INDEX_FILE)).unwrap();
        assert!(text.contains("pref-rust-error-style"));
        assert!(text.contains("preferences/rust-error-style.md"));
    }
}
