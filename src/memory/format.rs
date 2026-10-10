//! 记忆条目的 schema：frontmatter 的解析与序列化（`docs/memory.md` §3.2）。
//!
//! 一条记忆 = 一段 Markdown 文件，开头是 `---` 包裹的 frontmatter，其后是正文。
//!
//! 这里**不引入 YAML 依赖**：只支持我们约定的极小子集——`key: value` 标量行，
//! 以及 `source:` 下的缩进列表（`  - session: <ref>`）。契约窄，解析就简单可测；
//! 将来真要复杂结构再换 YAML 也不迟（别为想象的需求上依赖）。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 记忆模块错误。
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// 文件内容不合法（缺 frontmatter、缺必填字段、字段类型不对）。
    #[error("invalid memory entry: {0}")]
    Parse(String),
    /// 底层 IO 失败。
    #[error("memory io error: {0}")]
    Io(#[from] std::io::Error),
}

/// 条目类型（`docs/memory.md` §3.2）。
///
/// 对齐第 3 章认知科学三类长期记忆：`Event` 是情景，`Preference` / `Fact` 偏语义，
/// `Procedure` 是程序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    /// 偏好 / 习惯（"以后都这样"）。
    Preference,
    /// 事实（"张三是同事"、"项目用 Postgres"）。
    Fact,
    /// 事件（带时间戳的具体经历）。
    Event,
    /// 程序记忆（"先 X 再 Y"的行为流程）。
    Procedure,
}

impl EntryType {
    fn as_str(self) -> &'static str {
        match self {
            EntryType::Preference => "preference",
            EntryType::Fact => "fact",
            EntryType::Event => "event",
            EntryType::Procedure => "procedure",
        }
    }

    fn parse(value: &str) -> Result<Self, MemoryError> {
        match value {
            "preference" => Ok(EntryType::Preference),
            "fact" => Ok(EntryType::Fact),
            "event" => Ok(EntryType::Event),
            "procedure" => Ok(EntryType::Procedure),
            other => Err(MemoryError::Parse(format!("unknown type `{other}`"))),
        }
    }
}

/// 条目状态：版本化的关键——取代旧条目不删旧条目（`docs/memory.md` 决策 4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// 当前生效。
    Active,
    /// 已被更新的条目取代（旧条目保留，仅改状态）。
    Superseded,
    /// 证据不足、待确认（`confidence: low` 时常伴生）。
    Unconfirmed,
}

impl EntryStatus {
    fn as_str(self) -> &'static str {
        match self {
            EntryStatus::Active => "active",
            EntryStatus::Superseded => "superseded",
            EntryStatus::Unconfirmed => "unconfirmed",
        }
    }

    fn parse(value: &str) -> Result<Self, MemoryError> {
        match value {
            "active" => Ok(EntryStatus::Active),
            "superseded" => Ok(EntryStatus::Superseded),
            "unconfirmed" => Ok(EntryStatus::Unconfirmed),
            other => Err(MemoryError::Parse(format!("unknown status `{other}`"))),
        }
    }
}

/// 置信度：证据不足时降级标注（`docs/memory.md` §3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    fn as_str(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }

    fn parse(value: &str) -> Result<Self, MemoryError> {
        match value {
            "high" => Ok(Confidence::High),
            "medium" => Ok(Confidence::Medium),
            "low" => Ok(Confidence::Low),
            other => Err(MemoryError::Parse(format!("unknown confidence `{other}`"))),
        }
    }
}

/// 一条记忆条目。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// 稳定标识（如 `pref-rust-error-style`）。文件名与 `index.md` 均引用它。
    pub id: String,
    /// 分类。
    pub entry_type: EntryType,
    /// 主题（供检索 / 归并）。
    pub subject: String,
    /// 写入时间（`YYYY-MM-DD`）。
    pub created_at: String,
    /// 生效时间（`YYYY-MM-DD`）。时间线的关键：与 `created_at` 可不同。
    pub valid_from: Option<String>,
    /// 取代了哪条旧条目（旧条目不删，改 `status: superseded`）。
    pub supersedes: Option<String>,
    /// 版本化状态。
    pub status: EntryStatus,
    /// 置信度。
    pub confidence: Confidence,
    /// **适用场景**（qualification）：冲突双方可在不同条件下并存，不二选一。
    pub scope: Option<String>,
    /// 历史有用度（V2 排序用，V1 只存）。
    pub utility: Option<f64>,
    /// 被检索命中次数（V2 重要性评分，V1 只存）。
    pub usage_count: Option<u64>,
    /// 证据引用，可回溯到原始会话（`sessions/*.jsonl#turn:N`）。
    pub source: Vec<String>,
    /// 正文（Markdown）。
    pub body: String,
}

impl Entry {
    /// 解析一段完整条目文本（frontmatter + 正文）。
    pub fn parse(text: &str) -> Result<Self, MemoryError> {
        let (front, body) = split_frontmatter(text)?;
        let mut id = None;
        let mut entry_type = None;
        let mut subject = None;
        let mut created_at = None;
        let mut valid_from = None;
        let mut supersedes = None;
        let mut status = EntryStatus::Active;
        let mut confidence = Confidence::High;
        let mut scope = None;
        let mut utility = None;
        let mut usage_count = None;
        let mut source: Vec<String> = Vec::new();

        let mut in_source = false;
        for line in front.lines() {
            // `source:` 下的列表项：`  - session: <ref>`（也容忍 `  - <ref>`）。
            if in_source {
                let trimmed = line.trim_start();
                if let Some(item) = trimmed.strip_prefix("- ") {
                    source.push(normalize_source(item.trim()));
                    continue;
                }
                if !trimmed.is_empty() && !line.starts_with(char::is_whitespace) {
                    in_source = false; // 回到标量键
                } else {
                    continue;
                }
            }

            let Some((key, value)) = line.split_once(':') else {
                continue; // 忽略空行 / 无法识别的行（宽松解析）
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "id" => id = Some(value.to_string()),
                "type" => entry_type = Some(EntryType::parse(value)?),
                "subject" => subject = Some(value.to_string()),
                "created_at" => created_at = Some(value.to_string()),
                "valid_from" => valid_from = Some(value.to_string()),
                "supersedes" => supersedes = Some(value.to_string()),
                "status" => status = EntryStatus::parse(value)?,
                "confidence" => confidence = Confidence::parse(value)?,
                "scope" => scope = Some(value.to_string()),
                "utility" => utility = Some(parse_f64(key, value)?),
                "usage_count" => usage_count = Some(parse_u64(key, value)?),
                "source" => {
                    in_source = true;
                    // `source: []` 是"无证据"的空列表写法；行内单值
                    // `source: sessions/x.jsonl#turn:3` 也容忍。
                    if !value.is_empty() && value != "[]" {
                        source.push(normalize_source(value));
                    }
                }
                _ => {} // 未知键忽略，向前兼容
            }
        }

        Ok(Entry {
            id: require(id, "id")?,
            entry_type: require(entry_type, "type")?,
            subject: require(subject, "subject")?,
            created_at: require(created_at, "created_at")?,
            valid_from,
            supersedes,
            status,
            confidence,
            scope,
            utility,
            usage_count,
            source,
            body: body.trim().to_string(),
        })
    }

    /// 序列化回条目文本（frontmatter + 空行 + 正文）。
    pub fn render(&self) -> String {
        let mut out = String::from("---\n");
        out.push_str(&format!("id: {}\n", self.id));
        out.push_str(&format!("type: {}\n", self.entry_type.as_str()));
        out.push_str(&format!("subject: {}\n", self.subject));
        out.push_str(&format!("created_at: {}\n", self.created_at));
        if let Some(valid_from) = &self.valid_from {
            out.push_str(&format!("valid_from: {valid_from}\n"));
        }
        if let Some(supersedes) = &self.supersedes {
            out.push_str(&format!("supersedes: {supersedes}\n"));
        }
        out.push_str(&format!("status: {}\n", self.status.as_str()));
        out.push_str(&format!("confidence: {}\n", self.confidence.as_str()));
        if let Some(scope) = &self.scope {
            out.push_str(&format!("scope: {scope}\n"));
        }
        if let Some(utility) = self.utility {
            out.push_str(&format!("utility: {utility}\n"));
        }
        if let Some(usage_count) = self.usage_count {
            out.push_str(&format!("usage_count: {usage_count}\n"));
        }
        if self.source.is_empty() {
            out.push_str("source: []\n");
        } else {
            out.push_str("source:\n");
            for ref_ in &self.source {
                out.push_str(&format!("  - session: {ref_}\n"));
            }
        }
        out.push_str("---\n\n");
        out.push_str(&self.body);
        out.push('\n');
        out
    }
}

/// 一行索引（`index.md` 用）：从条目投影出的检索所需最小信息。
#[derive(Debug, Clone, PartialEq)]
pub struct IndexLine {
    pub id: String,
    pub entry_type: EntryType,
    /// 相对记忆根的路径（如 `preferences/rust-error-style.md`）。
    pub path: String,
    /// 一句话摘要（取正文首行，见 [`Entry::summary`]）。
    pub summary: String,
    /// 时间线展示用（`valid_from` 优先，回退 `created_at`）。
    pub date: String,
}

impl Entry {
    /// 正文首行作为摘要（供 `index.md` 与相关注入展示）。
    pub fn summary(&self) -> String {
        self.body
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim().trim_start_matches('#').trim().to_string())
            .unwrap_or_default()
    }

    /// 时间线展示日期：`valid_from` 优先，回退 `created_at`。
    pub fn timeline_date(&self) -> &str {
        self.valid_from.as_deref().unwrap_or(&self.created_at)
    }
}

fn split_frontmatter(text: &str) -> Result<(&str, &str), MemoryError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text); // 容忍 BOM
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(|| MemoryError::Parse("missing opening `---`".into()))?;
    // 找闭合的 `---` 行。
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare == "---" {
            let front = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Ok((front, body));
        }
        offset += line.len();
    }
    Err(MemoryError::Parse("missing closing `---`".into()))
}

fn normalize_source(raw: &str) -> String {
    raw.strip_prefix("session:").map(str::trim).unwrap_or(raw).to_string()
}

fn require<T>(value: Option<T>, field: &str) -> Result<T, MemoryError> {
    value.ok_or_else(|| MemoryError::Parse(format!("missing required field `{field}`")))
}

fn parse_f64(key: &str, value: &str) -> Result<f64, MemoryError> {
    value
        .parse()
        .map_err(|_| MemoryError::Parse(format!("field `{key}` is not a number: `{value}`")))
}

fn parse_u64(key: &str, value: &str) -> Result<u64, MemoryError> {
    value
        .parse()
        .map_err(|_| MemoryError::Parse(format!("field `{key}` is not an integer: `{value}`")))
}

impl fmt::Display for EntryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
---
id: pref-rust-error-style
type: preference
subject: rust-error-handling
created_at: 2026-05-10
valid_from: 2026-05-10
supersedes: pref-rust-error-style-v1
status: active
confidence: high
scope: rust / 领域错误处理
utility: 0.8
usage_count: 3
source:
  - session: 2026-05-10-abc.jsonl#turn:14
  - session: 2026-05-11-def.jsonl#turn:2
---

用户偏好用 thiserror 而非 anyhow 定义领域错误。
";

    #[test]
    fn parses_full_entry() {
        let entry = Entry::parse(SAMPLE).unwrap();
        assert_eq!(entry.id, "pref-rust-error-style");
        assert_eq!(entry.entry_type, EntryType::Preference);
        assert_eq!(entry.subject, "rust-error-handling");
        assert_eq!(entry.created_at, "2026-05-10");
        assert_eq!(entry.valid_from.as_deref(), Some("2026-05-10"));
        assert_eq!(entry.supersedes.as_deref(), Some("pref-rust-error-style-v1"));
        assert_eq!(entry.status, EntryStatus::Active);
        assert_eq!(entry.confidence, Confidence::High);
        assert_eq!(entry.scope.as_deref(), Some("rust / 领域错误处理"));
        assert_eq!(entry.utility, Some(0.8));
        assert_eq!(entry.usage_count, Some(3));
        assert_eq!(
            entry.source,
            vec![
                "2026-05-10-abc.jsonl#turn:14".to_string(),
                "2026-05-11-def.jsonl#turn:2".to_string(),
            ]
        );
        assert_eq!(entry.body, "用户偏好用 thiserror 而非 anyhow 定义领域错误。");
    }

    #[test]
    fn round_trips() {
        let entry = Entry::parse(SAMPLE).unwrap();
        let again = Entry::parse(&entry.render()).unwrap();
        assert_eq!(entry, again);
    }

    #[test]
    fn scope_with_colon_survives() {
        // scope 值里含 `:` 也不能被当作键值分隔（只切第一个 `:`）。
        let entry = Entry::parse(SAMPLE).unwrap();
        assert_eq!(entry.scope.as_deref(), Some("rust / 领域错误处理"));
    }

    #[test]
    fn minimal_entry_defaults() {
        let text = "\
---
id: fact-pg
type: fact
subject: db
created_at: 2026-05-12
---

项目用 Postgres。
";
        let entry = Entry::parse(text).unwrap();
        assert_eq!(entry.status, EntryStatus::Active);
        assert_eq!(entry.confidence, Confidence::High);
        assert!(entry.valid_from.is_none());
        assert!(entry.supersedes.is_none());
        assert!(entry.scope.is_none());
        assert!(entry.source.is_empty());
    }

    #[test]
    fn rejects_missing_required() {
        let text = "---\nid: x\ntype: fact\n---\nbody\n";
        let err = Entry::parse(text).unwrap_err();
        assert!(matches!(err, MemoryError::Parse(msg) if msg.contains("subject")));
    }

    #[test]
    fn rejects_unknown_type() {
        let text = "---\nid: x\ntype: bogus\nsubject: s\ncreated_at: 2026-05-12\n---\nbody\n";
        assert!(Entry::parse(text).is_err());
    }

    #[test]
    fn empty_source_renders_as_bracket() {
        let entry = Entry {
            id: "fact-x".into(),
            entry_type: EntryType::Fact,
            subject: "s".into(),
            created_at: "2026-05-12".into(),
            valid_from: None,
            supersedes: None,
            status: EntryStatus::Active,
            confidence: Confidence::High,
            scope: None,
            utility: None,
            usage_count: None,
            source: Vec::new(),
            body: "body".into(),
        };
        let rendered = entry.render();
        assert!(rendered.contains("source: []"));
        assert_eq!(Entry::parse(&rendered).unwrap(), entry);
    }

    #[test]
    fn summary_takes_first_nonempty_line() {
        let entry = Entry::parse(SAMPLE).unwrap();
        assert_eq!(entry.summary(), "用户偏好用 thiserror 而非 anyhow 定义领域错误。");
    }
}
