//! 索引页维护 + 轻量关键词检索（`docs/memory.md` §3.3 / §4.1）。
//!
//! 两件事：
//!
//! 1. **`index.md`**：程序维护的入口页，每条记忆一行（id / 类型 / 摘要 / 路径 / 日期）。
//!    第 3 章警告"纯文本平铺会退化成孤岛"——索引页就是入口。由 [`write_index`]
//!    从当前全部条目重新生成（幂等，不做增量）。
//! 2. **关键词检索**（V1）：按当前 query 给条目打分、取 top-k。**故意不做语义**——
//!    V1 验收只要求"关键词检索 + 按时间排序"，同义改写不敏感的问题留待 V2 的 BM25 /
//!    语义升级（`docs/memory.md` §11 缺口 3）。

use std::collections::HashSet;

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

/// 关键词检索：给每条条目打分，返回得分 > 0 的条目，按分数降序（并列按时间新→旧）。
///
/// 打分规则（V1，简单可解释）：
/// - query 与条目的 `subject` / `id` / 正文 / `scope` 全部小写后分词；
/// - 命中 `subject` 权重最高（主题最相关），其次正文，再次 `id` / `scope`；
/// - 关键词做**子串**匹配（对 CJK 友好：`rust` 命中 `rust-error-handling`）。
pub fn search(entries: &[(String, Entry)], query: &str, limit: usize) -> Vec<Scored> {
    let terms = tokenize(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<Scored> = entries
        .iter()
        .filter_map(|(path, entry)| {
            let score = score_entry(entry, &terms);
            (score > 0.0).then(|| Scored {
                entry: entry.clone(),
                path: path.clone(),
                score,
            })
        })
        .collect();
    // 分数降序；并列时时间新→旧（`docs/memory.md` 验收 3：检索按时间排序）。
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.entry.timeline_date().cmp(a.entry.timeline_date()))
    });
    scored.truncate(limit);
    scored
}

fn score_entry(entry: &Entry, terms: &HashSet<String>) -> f64 {
    let subject = entry.subject.to_lowercase();
    let id = entry.id.to_lowercase();
    let scope = entry.scope.as_deref().unwrap_or("").to_lowercase();
    let body = entry.body.to_lowercase();

    let mut score = 0.0;
    for term in terms {
        if subject.contains(term.as_str()) {
            score += 3.0;
        }
        if body.contains(term.as_str()) {
            score += 1.0;
        }
        if id.contains(term.as_str()) || scope.contains(term.as_str()) {
            score += 0.5;
        }
    }
    // 被取代的旧条目降权（保留可检索，但不优先）。
    if entry.status == super::format::EntryStatus::Superseded {
        score *= 0.5;
    }
    score
}

/// 极简分词：按非字母数字（含 CJK 视为字母）切分，去停用词，小写。
///
/// CJK 不按词切（无词典），把每个连续 CJK 串当一个 term——配合子串匹配即可用。
fn tokenize(text: &str) -> HashSet<String> {
    const STOP: [&str; 16] = [
        "the", "a", "an", "of", "to", "and", "or", "is", "are", "in", "on", "for", "with", "that",
        "this", "it",
    ];
    let mut terms = HashSet::new();
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

fn push_term(terms: &mut HashSet<String>, term: &str, stop: &[&str]) {
    // 单字符拉丁词无检索价值（CJK 单字保留，因为可能是有意义的）。
    let is_cjk = term.chars().any(|c| c as u32 >= 0x4E00);
    if term.chars().count() < 2 && !is_cjk {
        return;
    }
    if stop.contains(&term) {
        return;
    }
    terms.insert(term.to_string());
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
