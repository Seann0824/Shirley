//! 索引页维护 + 轻量关键词检索（`docs/memory.md` §3.3 / §4.1）。
//!
//! 两件事：
//!
//! 1. **`index.md`**：程序维护的入口页，每条记忆一行（id / 类型 / 摘要 / 路径 / 日期）。
//!    第 3 章警告"纯文本平铺会退化成孤岛"——索引页就是入口。由 [`write_index`]
//!    从当前全部条目重新生成（幂等，不做增量）。
//! 2. **检索**（V2：BM25）：按当前 query 给条目打分、取 top-k。**故意不做语义**——
//!    V2 用 BM25（IDF + 词频饱和 + 文档长度归一），比 V1 的固定权重子串打分更稳；语义 /
//!    嵌入检索留待 V3（`docs/memory.md` §10）。

use std::collections::{HashMap, HashSet};

use super::format::{Entry, IndexLine, MemoryError};
use super::store::{MemoryStore, INDEX_FILE};

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
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.entry.timeline_date().cmp(a.entry.timeline_date()))
            .then_with(|| a.entry.id.cmp(&b.entry.id))
    });
    scored.truncate(limit);
    scored
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

/// 查询分词：按非字母数字（含 CJK 视为字母）切分，去停用词，小写，**去重**。
///
/// CJK 不按词切（无词典），把每个连续 CJK 串当一个 term——配合 BM25 的字段加权
/// 词频即可用。
fn tokenize(text: &str) -> HashSet<String> {
    tokenize_counts(text).into_iter().collect()
}

/// 分词（**保留重复计数**，供 BM25 词频）。
fn tokenize_counts(text: &str) -> Vec<String> {
    const STOP: [&str; 16] = [
        "the", "a", "an", "of", "to", "and", "or", "is", "are", "in", "on", "for", "with", "that",
        "this", "it",
    ];
    let mut terms = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            push_term(&mut terms, &current, &STOP);
            current.clear();
        }
    }
    if !current.is_empty() {
        push_term(&mut terms, &current, &STOP);
    }
    terms
}

fn push_term(terms: &mut Vec<String>, term: &str, stop: &[&str]) {
    // 单字符拉丁词无检索价值（CJK 单字保留，因为可能是有意义的）。
    let is_cjk = term.chars().any(|c| c as u32 >= 0x4E00);
    if term.chars().count() < 2 && !is_cjk {
        return;
    }
    if stop.contains(&term) {
        return;
    }
    terms.push(term.to_string());
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
