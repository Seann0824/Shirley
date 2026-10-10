//! 索引页维护 + 轻量关键词检索（`docs/memory.md` §3.3 / §4.1）。
//!
//! 两件事：
//!
//! 1. **`index.md`**：程序维护的入口页，每条记忆一行（id / 类型 / 摘要 / 路径 / 日期）。
//!    第 3 章警告"纯文本平铺会退化成孤岛"——索引页就是入口。由 [`write_index`]
//!    从当前全部条目重新生成（幂等，不做增量）。
//! 2. **检索**：V2 用 BM25（IDF + 词频饱和 + 文档长度归一）；V2.5 叠加**语义向量腿**
//!    （余弦），两腿用 RRF 融合成 [`hybrid_search`]。**未配置 embedding 时自动退化为
//!    纯 BM25**（`docs/memory.md` §10）。

use std::collections::{HashMap, HashSet};

use super::format::{Entry, IndexLine, MemoryError};
use super::store::{MemoryStore, INDEX_FILE};
use super::vector::{cosine, VectorStore};

/// 检索时一条条目的得分与命中词。
#[derive(Debug, Clone)]
pub struct Scored {
    pub entry: Entry,
    /// 相对路径（供 `read_file` 取全文；V1 尚未接线，检索结果只用 `entry`）。
    #[allow(dead_code)]
    pub path: String,
    /// 命中得分（越高越相关）。
    pub score: f64,
}

/// 从一条条目投影出索引行。
pub fn index_line(store: &MemoryStore, path: &str, entry: &Entry) -> IndexLine {
    let _ = store;
    IndexLine {
        id: entry.id.clone(),
        entry_type: entry.entry_type,
        path: path.to_string(),
        summary: entry.summary(),
        date: entry.timeline_date().to_string(),
    }
}

/// 把全部条目重写成 `index.md`（落主根）。空库写一个带标题的空索引页。
pub fn write_index(store: &MemoryStore, entries: &[(String, Entry)]) -> Result<(), MemoryError> {
    store.ensure_layout()?;
    let mut lines: Vec<IndexLine> = entries
        .iter()
        .map(|(path, entry)| index_line(store, path, entry))
        .collect();
    // 稳定顺序：时间倒序（新的在前），同日期按 id。
    lines.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.id.cmp(&b.id)));

    let mut text = String::from("# Memory Index\n\n");
    text.push_str("> 由程序维护；每条记忆一行。改动条目后重新生成，勿手改。\n\n");
    if lines.is_empty() {
        text.push_str("_(no memories yet)_\n");
    } else {
        for line in &lines {
            text.push_str(&format!(
                "- [{}]({}) · {} · {} · {}\n",
                line.id,
                line.path,
                line.entry_type,
                line.summary,
                line.date
            ));
        }
    }
    std::fs::write(store.primary_root().join(INDEX_FILE), text)?;
    Ok(())
}

/// 检索：给每条条目打分，返回得分 > 0 的条目，按分数降序（并列按时间新→旧）。
///
/// **打分用 BM25**（`docs/memory.md` §10 V2）：词频饱和 + 文档长度归一 + IDF——
/// 罕见词权重高于常见词、长文档不因堆词而占优，比 V1 的固定权重子串打分更稳。
/// 字段差异保留为**加权词频**（`subject` 最高，正文次之，`id` / `scope` 最低），
/// 于是"主题命中优于正文命中"这条 V1 语义在 BM25 下依然成立。
///
/// 参数（经典 BM25）：`k1 = 1.2`（词频饱和速度）、`b = 0.75`（长度归一强度）。
/// 检索接口不变（`query → 相关条目`），升级不动架构。
///
/// **V2-D**：命中得分再乘一个温和的 `utility` 因子（`0.9 ~ 1.1`）——历史有用度高的条目
/// 在同等相关度下略微靠前，但**不压倒** BM25 相关性。`utility` 缺失按 `1.0`。
pub fn search(entries: &[(String, Entry)], query: &str, limit: usize) -> Vec<Scored> {
    let terms = tokenize(query);
    if terms.is_empty() {
        return Vec::new();
    }

    // 每篇文档的加权词频与长度。
    let docs: Vec<(HashMap<String, f64>, f64)> =
        entries.iter().map(|(_, entry)| doc_terms(entry)).collect();
    let n = docs.len() as f64;
    let avgdl = if docs.is_empty() {
        0.0
    } else {
        docs.iter().map(|(_, dl)| *dl).sum::<f64>() / n
    };

    // 文档频率（含该词的文档数）→ IDF。
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
            })
        })
        .collect();

    // 分数降序；并列时时间新→旧（`docs/memory.md` 验收 3），再按 id 稳定。
    sort_scored(&mut scored);
    scored.truncate(limit);
    scored
}

/// 语义检索腿：按**余弦相似度**给条目打分（需要预取好的 query 向量 + 已存条目向量）。
///
/// 只对**在 `vectors` 里有向量**的条目打分（没嵌入过的条目这条腿召回不了——这是
/// 混合检索的意义所在：BM25 腿仍然覆盖它们）。状态 / 有用度因子与 BM25 腿一致。
/// 返回相似度 > 0 的条目，按相似度降序（并列按时间新→旧，再按 id 稳定）。
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
            })
        })
        .collect();
    sort_scored(&mut scored);
    scored.truncate(limit);
    scored
}

/// RRF 常数（Reciprocal Rank Fusion）：`k = 60` 是经典取值。
const RRF_K: f64 = 60.0;
/// 每腿最少召回条数（融合前多召回一些，给另一腿补位的机会）。
const LEG_LIMIT_MIN: usize = 20;

/// **混合检索**：关键词 BM25 腿 + 语义向量腿，用 **RRF（Reciprocal Rank Fusion）** 融合。
///
/// 两腿各自召回 `max(limit*4, 20)` 条，按 `1/(k + rank)`（`k = 60`）累加分数。
/// RRF 只看**排名**、不看原始分数，天然免去"BM25 分数与余弦分数不同量纲怎么归一"
/// 的麻烦——这正是它成为混合检索标准做法的原因。
///
/// `query_vector` 为 `None`（未配置 embedding / 预取失败）时**退化为纯 BM25**，
/// 与升级前行为一致（诚实降级，不假装有语义腿）。
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
    // 语义腿没东西（未配置 / 无向量）→ 纯 BM25。
    if vector_hits.is_empty() {
        return bm25_hits.into_iter().take(limit).collect();
    }

    // 按 id 融合两腿的 RRF 分数。
    let mut fused: HashMap<String, Scored> = HashMap::new();
    for hits in [&bm25_hits, &vector_hits] {
        for (rank, hit) in hits.iter().enumerate() {
            let contribution = 1.0 / (RRF_K + (rank + 1) as f64);
            fused
                .entry(hit.entry.id.clone())
                .and_modify(|existing| existing.score += contribution)
                .or_insert_with(|| Scored {
                    entry: hit.entry.clone(),
                    path: hit.path.clone(),
                    score: contribution,
                });
        }
    }
    let mut scored: Vec<Scored> = fused.into_values().collect();
    sort_scored(&mut scored);
    scored.truncate(limit);
    scored
}

/// 统一的排序：分数降序，并列按时间新→旧，再按 id 稳定。
fn sort_scored(scored: &mut [Scored]) {
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.entry.timeline_date().cmp(a.entry.timeline_date()))
            .then_with(|| a.entry.id.cmp(&b.entry.id))
    });
}

/// 字段权重（相对正文）：主题最重要，`id` / `scope` 辅助。
const SUBJECT_WEIGHT: f64 = 3.0;
const ID_SCOPE_WEIGHT: f64 = 0.5;

/// 把一条条目分词成**加权词频**表 + 文档长度（权重和）。
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

/// BM25 打分（对唯一查询词求和）。
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

/// 状态因子：被取代的旧条目降权（保留可检索，但不优先）。
fn status_factor(status: super::format::EntryStatus) -> f64 {
    match status {
        super::format::EntryStatus::Superseded => 0.5,
        _ => 1.0,
    }
}

/// 有用度因子（V2-D）：`utility ∈ [0,1]` 映射到 `0.9 ~ 1.1` 的温和乘子；缺失按 `1.0`。
///
/// 刻意**只做温和调整**——相关性（BM25）仍是主序，`utility` 只在相关度接近时微调，
/// 不改变"关键词检索 + 时间并列"的 V1 验收口径。
fn utility_factor(utility: Option<f64>) -> f64 {
    match utility {
        Some(u) => 0.9 + 0.2 * u.clamp(0.0, 1.0),
        None => 1.0,
    }
}

/// 英文停用词（CJK 不做停用词过滤——bigram 已足够短）。
const STOP: [&str; 16] = [
    "the", "a", "an", "of", "to", "and", "or", "is", "are", "in", "on", "for", "with", "that",
    "this", "it",
];

/// 查询分词：拉丁按非字母数字切、去停用词、小写、**去重**；CJK 走 bigram（见下）。
fn tokenize(text: &str) -> HashSet<String> {
    tokenize_counts(text).into_iter().collect()
}

/// 分词（**保留重复计数**，供 BM25 词频）。
///
/// 两条腿：
/// - **拉丁 / 数字**：连续 `is_alphanumeric` 串当一个 term，去停用词、小写、长度 < 2 丢弃；
/// - **CJK**：连续 CJK 串按 **bigram** 切（相邻两字成 term，单字串保留该字）。
///   这修的是 V2 的一个真实缺陷：原先整段连续 CJK 被当成**一个** term，导致
///   「我的名字是什么」与条目「用户的名字是 Sean」永远匹配不上（中文自然语言查询
///   几乎必然 0 命中）。bigram 是零依赖、无词典的中文检索标准退而求其次做法，
///   与 BM25 的字段加权词频天然兼容。
fn tokenize_counts(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut run = String::new();
    let mut run_is_cjk = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            let is_cjk = is_cjk_char(ch);
            // 类别切换（拉丁 ↔ CJK）先冲刷上一段。
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

/// 一段同类字符（拉丁或 CJK）→ terms。
fn push_run(terms: &mut Vec<String>, run: &str, is_cjk: bool) {
    if is_cjk {
        push_cjk_bigrams(run, terms);
    } else if run.chars().count() >= 2 && !STOP.contains(&run) {
        terms.push(run.to_string());
    }
}

/// CJK 串按 bigram 切：相邻两字一个 term；单字串保留该字。
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

/// 是否 CJK 字符（含扩展区 / 兼容区 / 日文假名）——按 Unicode 码位粗判，够用即可。
fn is_cjk_char(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF      // 平假名 / 片假名
        | 0x3400..=0x4DBF    // CJK 扩展 A
        | 0x4E00..=0x9FFF    // CJK 基本区
        | 0xF900..=0xFAFF    // CJK 兼容
        | 0x20000..=0x2FA1F  // CJK 扩展 B~F
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::format::{Confidence, EntryStatus, EntryType};

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

    fn sample() -> Vec<(String, Entry)> {
        vec![
            (
                "preferences/rust-error-style.md".into(),
                entry(
                    "pref-rust-error-style",
                    "rust-error-handling",
                    EntryType::Preference,
                    "用户偏好用 thiserror 而非 anyhow 定义领域错误。",
                ),
            ),
            (
                "facts/db.md".into(),
                entry("fact-db", "database", EntryType::Fact, "项目用 Postgres。"),
            ),
            (
                "episodic/2026-05-12-ts.md".into(),
                entry(
                    "event-ts-migration",
                    "ts-migration",
                    EntryType::Event,
                    "2026-05-12 TS 后端迁移启动。",
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
        // 越界值被 clamp，不放大成异常乘子。
        assert!((utility_factor(Some(5.0)) - 1.1).abs() < 1e-9);
        assert!((utility_factor(Some(-3.0)) - 0.9).abs() < 1e-9);
    }

    #[test]
    fn utility_only_breaks_near_ties() {
        // 两条相关度相同的条目，utility 高的略靠前；但相关度差足够大时 utility 翻不了盘。
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
            (
                "old.md".into(),
                {
                    let mut e = entry("old", "topic", EntryType::Fact, "about topic");
                    e.valid_from = Some("2026-01-01".into());
                    e
                },
            ),
            (
                "new.md".into(),
                {
                    let mut e = entry("new", "topic", EntryType::Fact, "about topic");
                    e.valid_from = Some("2026-06-01".into());
                    e
                },
            ),
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
            (
                "old.md".into(),
                {
                    let mut e = entry("old", "topic", EntryType::Preference, "topic old");
                    e.status = EntryStatus::Superseded;
                    e
                },
            ),
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
        // query 与 a 同向 → a 应排第一。
        let hits = search_vector(&entries, &[1.0, 0.0], &vectors, 5);
        assert_eq!(hits[0].entry.id, "a");
        // 与两条都正交 → 无命中。
        assert!(search_vector(&entries, &[0.0, 0.0], &vectors, 5).is_empty());
    }

    #[test]
    fn hybrid_fuses_both_legs_via_rrf() {
        // a 只在 BM25 命中（关键词），b 只在向量腿命中（语义）——融合后两条都应在。
        let entries = vec![
            ("a.md".into(), entry("a", "note", EntryType::Fact, "thiserror")),
            ("b.md".into(), entry("b", "note", EntryType::Fact, "无关正文")),
        ];
        let mut vectors = VectorStore::new("m");
        vectors.upsert("a", vec![0.0, 1.0]);
        vectors.upsert("b", vec![1.0, 0.0]);
        let hits = hybrid_search(&entries, "thiserror", Some(&[1.0, 0.0]), &vectors, 5);
        let ids: Vec<&str> = hits.iter().map(|h| h.entry.id.as_str()).collect();
        assert!(ids.contains(&"a"), "BM25 腿应贡献 a: {ids:?}");
        assert!(ids.contains(&"b"), "向量腿应贡献 b: {ids:?}");
    }

    #[test]
    fn hybrid_without_query_vector_degrades_to_bm25() {
        let entries = vec![(
            "a.md".into(),
            entry("a", "note", EntryType::Fact, "thiserror"),
        )];
        let vectors = VectorStore::new("m");
        // 无 query 向量 → 纯 BM25（结果与 search 一致）。
        let hybrid = hybrid_search(&entries, "thiserror", None, &vectors, 5);
        let bm25 = search(&entries, "thiserror", 5);
        assert_eq!(hybrid.len(), bm25.len());
        assert_eq!(hybrid[0].entry.id, bm25[0].entry.id);
    }

    #[test]
    fn cjk_natural_language_query_matches_via_bigram() {
        // 回归：原先整段连续 CJK 被当成**一个** term，导致自然语言提问（与条目用词不同）
        // 永远 0 命中。bigram 后「我的名字是什么」应能命中「用户的名字是 Sean。」（共享
        // 的名 / 名字 / 字是 等 bigram）。
        let entries = vec![(
            "n.md".into(),
            entry("n", "user-identity", EntryType::Fact, "用户的名字是 Sean。"),
        )];
        let hits = search(&entries, "我的名字是什么", 5);
        assert_eq!(hits.len(), 1, "自然语言中文查询应命中（bigram）");
        assert_eq!(hits[0].entry.id, "n");
    }

    #[test]
    fn tokenize_splits_cjk_into_bigrams_and_latin_words() {
        let terms = tokenize("用 Rust 写 thiserror 错误");
        assert!(terms.contains("rust"), "拉丁词整词保留");
        assert!(terms.contains("thiserror"));
        assert!(terms.contains("错误"), "两字 CJK 串 = 一个 bigram");
        assert!(
            !terms.contains("thiserror错误"),
            "拉丁与 CJK 属于不同 run，不应拼成一个 term"
        );
    }

    #[test]
    fn bm25_rare_term_outweighs_common() {
        // rust 出现在两篇，thiserror 只在一篇：query 同时含两者时，含罕见词的那篇应排前。
        let entries = vec![
            (
                "a.md".into(),
                entry("a", "rust", EntryType::Fact, "thiserror"),
            ),
            (
                "b.md".into(),
                entry("b", "rust", EntryType::Fact, "anyhow"),
            ),
        ];
        let hits = search(&entries, "rust thiserror", 5);
        assert_eq!(hits[0].entry.id, "a", "rare term should outweigh common term");
    }

    #[test]
    fn bm25_length_normalization_penalizes_long_docs() {
        // 同样命中一次，短文档应排在长文档之前（长度归一）。
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
        let store = MemoryStore::new(&root);
        let entries: Vec<(String, Entry)> = sample();
        write_index(&store, &entries).unwrap();
        let text = std::fs::read_to_string(root.join(INDEX_FILE)).unwrap();
        assert!(text.contains("pref-rust-error-style"));
        assert!(text.contains("preferences/rust-error-style.md"));
    }
}
