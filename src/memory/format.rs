//! Memory entry schema: parsing and serializing frontmatter (`docs/memory.md` 3.2).
//!
//! One memory = a Markdown file that starts with frontmatter wrapped in `---`, followed by the body.
//!
//! This **does not pull in a YAML dependency**: it only supports the minimal subset we agreed on -- `key: value` scalar lines,
//! plus an indented list under `source:` (`  - session: <ref>`). A narrow contract keeps parsing simple and testable;
//! if complex structures are truly needed later, switching to YAML is not too late (do not add dependencies for imagined needs).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Memory module error.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// File content is invalid (missing frontmatter, missing required field, wrong field type).
    #[error("invalid memory entry: {0}")]
    Parse(String),
    /// Underlying IO failure.
    #[error("memory io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Entry type (`docs/memory.md` 3.2).
///
/// Aligns with the three cognitive-science long-term memory categories from chapter 3: `Event` is episodic, `Preference` / `Fact` are more semantic,
/// and `Procedure` is procedural.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    /// Preference / habit ("do it this way from now on").
    Preference,
    /// Fact ("Zhang San is a colleague", "the project uses Postgres").
    Fact,
    /// Event (a specific experience with a timestamp).
    Event,
    /// Procedural memory (a "first X then Y" behavior flow).
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

/// Entry status: the key to versioning -- superseding an old entry does not delete it (`docs/memory.md` decision 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// Currently in effect.
    Active,
    /// Superseded by an updated entry (the old entry is kept, only its status changes).
    Superseded,
    /// Insufficient evidence, pending confirmation (often accompanies `confidence: low`).
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

/// Confidence: a downgrade label when evidence is insufficient (`docs/memory.md` 3.2).
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

/// Memory owning domain -- an axis **orthogonal** to [`EntryType`] (semantic type).
///
/// The semantic type answers "what is this" (episodic/semantic/procedural); the owning domain answers "whose is it" (personal/project).
/// The owning domain is carried by the **storage root**: `Personal` lands in the global root (cross-project, travels with the user),
/// `Project` lands in the workspace root (travels with the repo, each workspace naturally isolated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    /// Personal memory: user profile / preferences / cross-project facts -> global root.
    ///
    /// Default value (`#[default]`): backward compatible -- old entries without a `domain` field are treated as personal memory
    /// (conservative, landing in the global root).
    #[default]
    Personal,
    /// Project memory: facts / flows specific to a workspace -> workspace root (each workspace isolated).
    Project,
}

impl Domain {
    fn as_str(self) -> &'static str {
        match self {
            Domain::Personal => "personal",
            Domain::Project => "project",
        }
    }

    fn parse(value: &str) -> Result<Self, MemoryError> {
        match value {
            "personal" => Ok(Domain::Personal),
            "project" => Ok(Domain::Project),
            other => Err(MemoryError::Parse(format!("unknown domain `{other}`"))),
        }
    }
}

/// One memory entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Stable identifier (e.g. `pref-rust-error-style`). Both the file name and `index.md` reference it.
    pub id: String,
    /// Category.
    pub entry_type: EntryType,
    /// Owning domain: personal (global root) / project (workspace root). Determines which storage root the entry lands in.
    pub domain: Domain,
    /// Subject (for retrieval / merging).
    pub subject: String,
    /// Write time (`YYYY-MM-DD`).
    pub created_at: String,
    /// Effective time (`YYYY-MM-DD`). The key to the timeline: may differ from `created_at`.
    pub valid_from: Option<String>,
    /// Which old entry this supersedes (the old entry is not deleted; change `status: superseded`).
    pub supersedes: Option<String>,
    /// Versioned status.
    pub status: EntryStatus,
    /// Confidence.
    pub confidence: Confidence,
    /// **Applicable scope** (qualification): the two sides of a conflict can coexist under different conditions, not an either/or.
    pub scope: Option<String>,
    /// Historical usefulness (used by V2 ordering, only stored in V1).
    pub utility: Option<f64>,
    /// Number of times retrieved (V2 importance scoring, only stored in V1).
    pub usage_count: Option<u64>,
    /// Evidence references, traceable back to the original session (`sessions/*.jsonl#turn:N`).
    pub source: Vec<String>,
    /// Body (Markdown).
    pub body: String,
}

impl Entry {
    /// Parse a full entry text (frontmatter + body).
    pub fn parse(text: &str) -> Result<Self, MemoryError> {
        let (front, body) = split_frontmatter(text)?;
        let mut id = None;
        let mut entry_type = None;
        let mut domain = Domain::Personal;
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
            // List items under `source:`: `  - session: <ref>` (also tolerates `  - <ref>`).
            if in_source {
                let trimmed = line.trim_start();
                if let Some(item) = trimmed.strip_prefix("- ") {
                    source.push(normalize_source(item.trim()));
                    continue;
                }
                if !trimmed.is_empty() && !line.starts_with(char::is_whitespace) {
                    in_source = false; // back to a scalar key
                } else {
                    continue;
                }
            }

            let Some((key, value)) = line.split_once(':') else {
                continue; // ignore blank / unrecognized lines (lenient parsing)
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "id" => id = Some(value.to_string()),
                "type" => entry_type = Some(EntryType::parse(value)?),
                "domain" => domain = Domain::parse(value)?,
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
                    // `source: []` is the "no evidence" empty-list form; an inline single value
                    // `source: sessions/x.jsonl#turn:3` is also tolerated.
                    if !value.is_empty() && value != "[]" {
                        source.push(normalize_source(value));
                    }
                }
                _ => {} // unknown keys ignored, forward compatible
            }
        }

        Ok(Entry {
            id: require(id, "id")?,
            entry_type: require(entry_type, "type")?,
            domain,
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

    /// Serialize back to entry text (frontmatter + blank line + body).
    pub fn render(&self) -> String {
        let mut out = String::from("---\n");
        out.push_str(&format!("id: {}\n", self.id));
        out.push_str(&format!("type: {}\n", self.entry_type.as_str()));
        out.push_str(&format!("domain: {}\n", self.domain.as_str()));
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

pub(crate) fn has_explicit_domain(text: &str) -> bool {
    split_frontmatter(text).is_ok_and(|(front, _)| {
        front.lines().any(|line| {
            line.split_once(':')
                .is_some_and(|(key, _)| key.trim() == "domain")
        })
    })
}

/// One index line (used by `index.md`): the minimal retrieval info projected from an entry.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexLine {
    pub id: String,
    pub entry_type: EntryType,
    /// Path relative to the memory root (e.g. `preferences/rust-error-style.md`).
    pub path: String,
    /// One-line summary (first line of the body, see [`Entry::summary`]).
    pub summary: String,
    /// For timeline display (`valid_from` preferred, falling back to `created_at`).
    pub date: String,
}

impl Entry {
    /// First line of the body as the summary (for `index.md` and related injection display).
    pub fn summary(&self) -> String {
        self.body
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim().trim_start_matches('#').trim().to_string())
            .unwrap_or_default()
    }

    /// Timeline display date: `valid_from` preferred, falling back to `created_at`.
    pub fn timeline_date(&self) -> &str {
        self.valid_from.as_deref().unwrap_or(&self.created_at)
    }
}

fn split_frontmatter(text: &str) -> Result<(&str, &str), MemoryError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text); // tolerate BOM
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(|| MemoryError::Parse("missing opening `---`".into()))?;
    // find the closing `---` line.
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
    raw.strip_prefix("session:")
        .map(str::trim)
        .unwrap_or(raw)
        .to_string()
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
scope: rust / domain-error-handling
utility: 0.8
usage_count: 3
source:
  - session: 2026-05-10-abc.jsonl#turn:14
  - session: 2026-05-11-def.jsonl#turn:2
---

The user prefers thiserror over anyhow for defining domain errors.
";

    #[test]
    fn parses_full_entry() {
        let entry = Entry::parse(SAMPLE).unwrap();
        assert_eq!(entry.id, "pref-rust-error-style");
        assert_eq!(entry.entry_type, EntryType::Preference);
        assert_eq!(entry.subject, "rust-error-handling");
        assert_eq!(entry.created_at, "2026-05-10");
        assert_eq!(entry.valid_from.as_deref(), Some("2026-05-10"));
        assert_eq!(
            entry.supersedes.as_deref(),
            Some("pref-rust-error-style-v1")
        );
        assert_eq!(entry.status, EntryStatus::Active);
        assert_eq!(entry.confidence, Confidence::High);
        assert_eq!(entry.scope.as_deref(), Some("rust / domain-error-handling"));
        assert_eq!(entry.utility, Some(0.8));
        assert_eq!(entry.usage_count, Some(3));
        assert_eq!(
            entry.source,
            vec![
                "2026-05-10-abc.jsonl#turn:14".to_string(),
                "2026-05-11-def.jsonl#turn:2".to_string(),
            ]
        );
        assert_eq!(
            entry.body,
            "The user prefers thiserror over anyhow for defining domain errors."
        );
    }

    #[test]
    fn round_trips() {
        let entry = Entry::parse(SAMPLE).unwrap();
        let again = Entry::parse(&entry.render()).unwrap();
        assert_eq!(entry, again);
    }

    #[test]
    fn scope_with_colon_survives() {
        // a `:` in the scope value must not be treated as a key-value separator (only the first `:` is split).
        let entry = Entry::parse(SAMPLE).unwrap();
        assert_eq!(entry.scope.as_deref(), Some("rust / domain-error-handling"));
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

The project uses Postgres.
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
            domain: Domain::Personal,
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
        assert_eq!(
            entry.summary(),
            "The user prefers thiserror over anyhow for defining domain errors."
        );
    }
}
