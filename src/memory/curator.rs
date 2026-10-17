//! Incremental curator after a session ends (`docs/memory.md` 5).
//!
//! The write path does not rely on "the AI proactively calling a tool" (that depends on the model's mood); instead, **when a session ends** the curator reads
//! the transcript and produces the smallest possible diff (new entries / supersede relations), which then goes through a **deterministic self-check** before being merged in.
//!
//! V1's three trade-offs (5.3):
//! - **Same model**: uses [`Agent::complete`] (one-shot, no tools, non-streaming, does not touch the session log).
//!   Cross-source review (a separate model) is left to V2.
//! - **Deterministic self-check**: no LLM; only checks schema / `source` / `supersedes` / time / links.
//!   **If it fails, it is not merged in -- never pass by default**.
//! - **Honest degradation**: read-only probing (5.4) is not done in V1, degrading to pure-transcript curation.
//!
//! Boundary statement (copying the paper's distiller boundary): the transcript is **partial evidence**, not ground truth --
//! one pass does not automatically validate every intermediate assumption. This is written into the prompt given to the model.
//!
//! **V2 adds periodic consolidation (sleep learning, `docs/memory.md` 5.2 / 6)**: [`consolidate`] scans the whole
//! memory store and has the model produce merge / supersede / requalify operations, each going through a **deterministic self-check** before execution,
//! and **checks back against the raw evidence** (feeding the `sessions/*.jsonl` snippets the entries' `source` points to into the prompt).
//! Conflicts are **not forcibly converged** -- if conditions differ, `requalify` adds `scope`; if evidence is insufficient, keep as is.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use shirley_agent_sdk::{Agent, Message};

use crate::prompt_template::render;

use super::format::{Confidence, Domain, Entry, EntryStatus, EntryType, MemoryError};
use super::store::MemoryStore;

/// Curator error.
#[derive(Debug, thiserror::Error)]
pub enum CuratorError {
    /// Model call failed (collapsed to a string to avoid the curator depending on the SDK's error type).
    #[error("curator model call failed: {0}")]
    Model(String),
    /// Model output is not valid JSON.
    #[error("curator output not parseable: {0}")]
    Parse(String),
    /// Persisting to disk failed.
    #[error(transparent)]
    Memory(#[from] MemoryError),
}

/// Result of one curation.
#[derive(Debug, Default)]
pub struct CurateOutcome {
    /// Ids of entries successfully written.
    pub written: Vec<String>,
    /// Candidates rejected by the deterministic self-check (id may be missing).
    pub rejected: Vec<Rejected>,
    /// The model produced no candidates (normal: nothing worth recording this round).
    pub empty: bool,
}

/// A rejected candidate and the reason.
///
/// For upper-layer observation (`spawn_curation` prints counts); V1 has no consumer yet, fields kept for diagnostics.
#[derive(Debug)]
#[allow(dead_code)]
pub struct Rejected {
    pub id: Option<String>,
    pub reason: String,
}

/// A candidate entry output by the model (decoupled from [`Entry`]: the model only provides these fields, the rest is filled by the curator).
#[derive(Debug, Clone, Deserialize)]
pub struct Candidate {
    pub id: String,
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    /// Owning domain: `personal` (default, personal/global) | `project` (project/workspace). Defaults to personal.
    #[serde(default)]
    pub domain: Domain,
    pub subject: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub confidence: Option<Confidence>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub source: Vec<String>,
    #[serde(default)]
    pub body: String,
}

/// Extract the candidate array from the model text (tolerates ```json code fences and surrounding chatter).
pub fn parse_candidates(text: &str) -> Result<Vec<Candidate>, CuratorError> {
    let start = text
        .find('[')
        .ok_or_else(|| CuratorError::Parse("no JSON array found".into()))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| CuratorError::Parse("no closing `]`".into()))?;
    if end < start {
        return Err(CuratorError::Parse("malformed JSON array".into()));
    }
    serde_json::from_str(&text[start..=end])
        .map_err(|err| CuratorError::Parse(err.to_string()))
}

/// Deterministic self-check + filling in default fields. On pass returns an [`Entry`], otherwise returns the rejection reason.
///
/// Check items (5.3, no LLM):
/// - `id` / `subject` / `body` non-empty, `id` is a slug;
/// - time fields are `YYYY-MM-DD`;
/// - `source` non-empty (**stamp**: the transcript already came from this session; when missing, fill in the known `session_ref`,
///   this completes the real source, not fabricates evidence); when `sessions_dir` is given, also validates that the source file exists;
/// - the id `supersedes` points to actually exists.
pub fn vet(
    candidate: Candidate,
    existing: &HashMap<String, Entry>,
    today: &str,
    session_ref: Option<&str>,
    sessions_dir: Option<&Path>,
) -> Result<Entry, String> {
    let id = candidate.id.trim();
    if id.is_empty() {
        return Err("empty id".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("id `{id}` is not a slug"));
    }
    if candidate.subject.trim().is_empty() {
        return Err("empty subject".into());
    }
    if candidate.body.trim().is_empty() {
        return Err("empty body".into());
    }

    let created_at = candidate
        .created_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(today)
        .to_string();
    if !is_date(&created_at) {
        return Err(format!("created_at `{created_at}` is not YYYY-MM-DD"));
    }
    if let Some(valid_from) = candidate.valid_from.as_deref()
        && !is_date(valid_from.trim())
    {
        return Err(format!("valid_from `{valid_from}` is not YYYY-MM-DD"));
    }

    let mut source: Vec<String> = candidate
        .source
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if source.is_empty()
        && let Some(session_ref) = session_ref
    {
        source.push(session_ref.to_string());
    }
    if source.is_empty() {
        return Err("missing source".into());
    }
    if let Some(dir) = sessions_dir {
        for ref_ in &source {
            let file = ref_.split('#').next().unwrap_or(ref_);
            if !dir.join(file).exists() {
                return Err(format!("source `{ref_}` does not resolve to an existing file"));
            }
        }
    }

    if let Some(supersedes) = candidate.supersedes.as_deref() {
        let supersedes = supersedes.trim();
        if supersedes.is_empty() {
            return Err("empty supersedes".into());
        }
        if supersedes == id {
            return Err("entry supersedes itself".into());
        }
        if !existing.contains_key(supersedes) {
            return Err(format!("supersedes `{supersedes}` does not exist"));
        }
    }

    Ok(Entry {
        id: id.to_string(),
        entry_type: candidate.entry_type,
        domain: candidate.domain,
        subject: candidate.subject.trim().to_string(),
        created_at,
        valid_from: candidate
            .valid_from
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        supersedes: candidate
            .supersedes
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        status: EntryStatus::Active,
        confidence: candidate.confidence.unwrap_or(Confidence::Medium),
        scope: candidate
            .scope
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        utility: None,
        usage_count: None,
        source,
        body: candidate.body.trim().to_string(),
    })
}

fn is_date(value: &str) -> bool {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

/// Render one message into a line the curator can read (skip system / compression summaries -- not conversation content).
pub fn render_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        match message {
            Message::User { content } => {
                out.push_str("## User\n");
                out.push_str(content.as_text().trim());
                out.push_str("\n\n");
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if let Some(content) = content.as_deref().filter(|c| !c.trim().is_empty()) {
                    out.push_str("## Assistant\n");
                    out.push_str(content.trim());
                    out.push_str("\n\n");
                }
                for call in tool_calls {
                    out.push_str(&format!("## Tool call: {}\n{}\n\n", call.name, call.arguments));
                }
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                out.push_str(&format!("## Tool result ({tool_call_id})\n"));
                if let Some(content) = content.as_ref() {
                    out.push_str(content.as_text().trim());
                }
                out.push('\n');
                out.push('\n');
            }
            Message::System { .. } | Message::ContextSummary { .. } => {}
        }
    }
    out
}

/// Incremental curator prompt template (`prompts/curator_incremental.md`).
const CURATOR_INCREMENTAL_TEMPLATE: &str = include_str!("../../prompts/curator_incremental.md");
/// Existing related memories section template.
const CURATOR_EXISTING_TEMPLATE: &str = include_str!("../../prompts/curator_existing.md");
/// Consolidate prompt template.
const CURATOR_CONSOLIDATE_TEMPLATE: &str = include_str!("../../prompts/curator_consolidate.md");
/// Raw evidence section template.
const CURATOR_EVIDENCE_TEMPLATE: &str = include_str!("../../prompts/curator_evidence.md");

/// Assemble the curator prompt: boundary statement + existing related memories (summaries) + transcript + output format.
pub fn build_prompt(transcript: &str, existing: &[(String, Entry)], session_ref: &str) -> String {
    let existing_section = if existing.is_empty() {
        String::new()
    } else {
        let mut entries = String::new();
        for (_, entry) in existing {
            entries.push_str(&format!(
                "- [{}] {} — {}\n",
                entry.id,
                entry.subject,
                entry.summary()
            ));
        }
        let mut section = render(CURATOR_EXISTING_TEMPLATE, &[("entries", &entries)]);
        if !section.ends_with('\n') {
            section.push('\n');
        }
        section
    };

    render(
        CURATOR_INCREMENTAL_TEMPLATE,
        &[
            ("session_ref", session_ref),
            ("existing_section", &existing_section),
            ("transcript", transcript),
        ],
    )
}

/// Trigger one incremental curation at session end.
///
/// - `agent`: used to call the model (same model, one-shot completion);
/// - `store`: memory store (read existing entries, write new entries, rebuild the index);
/// - `messages`: all messages of this session (the curator's input transcript);
/// - `session_ref`: this session's relative file name under `sessions/` (used as the `source` prefix).
pub async fn curate(
    agent: &Agent,
    store: &MemoryStore,
    messages: &[Message],
    session_ref: &str,
    sessions_dir: Option<&Path>,
) -> Result<CurateOutcome, CuratorError> {
    let existing_pairs: Vec<(String, Entry)> = store
        .list_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|(path, entry)| (store.relative_path(&path), entry))
        .collect();
    let existing_by_id: HashMap<String, Entry> = existing_pairs
        .iter()
        .map(|(_, entry)| (entry.id.clone(), entry.clone()))
        .collect();

    let transcript = render_transcript(messages);
    if transcript.trim().is_empty() {
        return Ok(CurateOutcome {
            empty: true,
            ..Default::default()
        });
    }
    let prompt = build_prompt(&transcript, &existing_pairs, session_ref);

    let raw = agent
        .complete(&prompt)
        .await
        .map_err(|err| CuratorError::Model(err.to_string()))?;
    let candidates = parse_candidates(&raw)?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut outcome = CurateOutcome {
        empty: candidates.is_empty(),
        ..Default::default()
    };

    for candidate in candidates {
        let candidate_id = Some(candidate.id.clone());
        match vet(
            candidate,
            &existing_by_id,
            &today,
            Some(session_ref),
            sessions_dir,
        ) {
            Ok(entry) => {
                // Supersede: keep the old entry, only change its status (`docs/memory.md` decision 4: never delete history).
                if let Some(old_id) = entry.supersedes.clone()
                    && let Some(old) = existing_by_id.get(&old_id)
                {
                    let mut old = old.clone();
                    if old.status != EntryStatus::Superseded {
                        old.status = EntryStatus::Superseded;
                        store.write_entry(&old)?;
                    }
                }
                store.write_entry(&entry)?;
                outcome.written.push(entry.id.clone());
            }
            Err(reason) => outcome.rejected.push(Rejected {
                id: candidate_id,
                reason,
            }),
        }
    }

    rebuild_index(store)?;
    rebuild_core(store)?;
    Ok(outcome)
}

/// Rebuild `index.md` from all current entries (called after the curator merges in).
pub fn rebuild_index(store: &MemoryStore) -> Result<(), MemoryError> {
    // **Rebuild per root**: global (personal) and workspace (project) each write their own `index.md` --
    // the index only includes entries of this root; it does not mix project entries into the global index (or vice versa).
    for root in store.roots() {
        let entries: Vec<(String, Entry)> = store
            .list_entries_in(root)?
            .into_iter()
            .map(|(path, entry)| (store.relative_path(&path), entry))
            .collect();
        super::index::write_index(root, &entries)?;
    }
    Ok(())
}

/// Hard cap of the `core.md` resident layer (characters; ~500 tokens, `docs/memory.md` 4.3).
pub const MAX_CORE_CHARS: usize = 2000;

/// Deterministically rebuild `core.md` from **active entries** (resident layer: profile + explicit preferences + active projects).
///
/// **No LLM**: the resident layer must be small and stable (prefix-cache-friendly); the program can project it from entries by fixed rules,
/// so it is not worth an extra model call, nor should it drift with the model's mood. Rules:
///
/// - only take entries with `status: active` (`superseded` / `unconfirmed` do not enter the resident layer);
/// - fixed order: `preference` -> `fact` -> `procedure` -> `event` (profile / preferences first);
/// - one line per entry `- [type] <summary>`, within the same type newest->oldest, then stable by id;
/// - truncate beyond [`MAX_CORE_CHARS`] and mark explicitly (prevents the resident layer from bloating itself, `docs/memory.md` 4.3).
///
/// When the store is empty, delete the old `core.md` (avoids a stale profile being injected further).
pub fn rebuild_core(store: &MemoryStore) -> Result<(), MemoryError> {
    // **Rebuild per root**: each root only projects the active entries **within that root**. The global `core.md` contains only personal
    // memory, the workspace `core.md` only project memory -- at injection time [`MemoryStore::read_core`] concatenates in
    // global->workspace order, so the two resident layers do not pollute each other.
    for root in store.roots() {
        let mut entries: Vec<Entry> = store
            .list_entries_in(root)?
            .into_iter()
            .map(|(_, entry)| entry)
            .filter(|entry| entry.status == EntryStatus::Active)
            .collect();

        let path = root.join(super::store::CORE_FILE);
        if entries.is_empty() {
            let _ = std::fs::remove_file(&path);
            continue;
        }

        // Fixed type order (profile / preferences first, events last), within a group newest->oldest, then by id.
        let rank = |entry_type: EntryType| match entry_type {
            EntryType::Preference => 0,
            EntryType::Fact => 1,
            EntryType::Procedure => 2,
            EntryType::Event => 3,
        };
        entries.sort_by(|a, b| {
            rank(a.entry_type)
                .cmp(&rank(b.entry_type))
                .then_with(|| b.timeline_date().cmp(a.timeline_date()))
                .then_with(|| a.id.cmp(&b.id))
        });

        let mut text = String::from("# Memory Core\n\n> Maintained by the program (projection of active entries); do not edit by hand.\n\n");
        let mut truncated = false;
        for entry in &entries {
            let line = format!(
                "- [{}] {}\n",
                super::provider::type_label(entry.entry_type),
                entry.summary()
            );
            if text.chars().count() + line.chars().count() > MAX_CORE_CHARS {
                truncated = true;
                break;
            }
            text.push_str(&line);
        }
        if truncated {
            text.push_str("\n_(truncated; see index.md for the full list)_\n");
        }

        store.write_core_at(root, &text)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// V2-B: periodic consolidation (sleep learning, `docs/memory.md` 5.2 / 6)
// ---------------------------------------------------------------------------

/// Cap on raw evidence snippet injection (prevents evidence from monopolizing the prompt).
pub const MAX_EVIDENCE_CHARS: usize = 6000;

/// Deterministic result of one periodic consolidation.
#[derive(Debug, Default)]
pub struct ConsolidateOutcome {
    /// Number of merge operations successfully executed.
    pub merged: usize,
    /// Number of old entries set to superseded.
    pub superseded: usize,
    /// Number of requalify operations successfully executed.
    pub requalified: usize,
    /// Operations rejected by the self-check.
    pub rejected: Vec<Rejected>,
    /// Nothing to consolidate (too few entries or the model produced no operations).
    pub empty: bool,
}

/// One consolidation operation (proposed by the model, executed after the program's self-check). **Never deletes history** -- only sets `superseded`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ConsolidateOp {
    /// Merge `sources` into one new `target` entry; all old entries are set to superseded.
    Merge {
        target: Candidate,
        sources: Vec<String>,
    },
    /// Supersede `id` with `replacement` (set `id` to superseded).
    Supersede {
        id: String,
        replacement: Candidate,
    },
    /// Change only one entry's applicable scenario / body (add qualification), without creating a new entry.
    Requalify {
        id: String,
        #[serde(default)]
        scope: Option<String>,
        #[serde(default)]
        body: Option<String>,
    },
}

/// Extract the consolidation operation array from the model text (tolerates ```json fences and surrounding chatter).
pub fn parse_consolidate_ops(text: &str) -> Result<Vec<ConsolidateOp>, CuratorError> {
    let start = text
        .find('[')
        .ok_or_else(|| CuratorError::Parse("no JSON array found".into()))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| CuratorError::Parse("no closing `]`".into()))?;
    if end < start {
        return Err(CuratorError::Parse("malformed JSON array".into()));
    }
    serde_json::from_str(&text[start..=end]).map_err(|err| CuratorError::Parse(err.to_string()))
}

/// Check back against the raw evidence (5.2): extract the session snippets that each entry's `source` points to, for the consolidation prompt.
///
/// `source` is of the form `<session file>#turn:N`: if `#turn:N` is given, take that line (1-based); otherwise take the whole file
/// truncated. A missing file is recorded as `[missing evidence]` -- **do not fabricate evidence**.
pub fn collect_evidence(entries: &[(String, Entry)], sessions_dir: &Path, budget: usize) -> String {
    let mut out = String::new();
    for (_, entry) in entries {
        if out.chars().count() >= budget {
            break;
        }
        out.push_str(&format!("### {}\n", entry.id));
        for ref_ in &entry.source {
            if out.chars().count() >= budget {
                break;
            }
            out.push_str(&format!("- {}: ", ref_));
            out.push_str(&evidence_snippet(ref_, sessions_dir));
            out.push('\n');
        }
    }
    out
}

/// Extract the session snippet corresponding to one evidence reference.
fn evidence_snippet(ref_: &str, sessions_dir: &Path) -> String {
    let mut parts = ref_.splitn(2, '#');
    let file = parts.next().unwrap_or(ref_).trim();
    let turn = parts
        .next()
        .and_then(|t| t.strip_prefix("turn:"))
        .and_then(|n| n.trim().parse::<usize>().ok());
    let path = sessions_dir.join(file);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return "[missing evidence]".into();
    };
    match turn {
        Some(n) if n >= 1 => text
            .lines()
            .nth(n - 1)
            .unwrap_or("[turn out of range]")
            .to_string(),
        _ => truncate(&text, 800),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Assemble the consolidation prompt: boundary statement + all entries + raw evidence + output format.
pub fn build_consolidate_prompt(entries: &[(String, Entry)], evidence: &str) -> String {
    let mut entries_text = String::new();
    for (_, entry) in entries {
        entries_text.push_str(&format!(
            "- [{}] ({}) {} — {}（{}）\n",
            entry.id,
            entry.entry_type,
            entry.subject,
            entry.summary(),
            entry.timeline_date()
        ));
    }

    let evidence_section = if evidence.trim().is_empty() {
        String::new()
    } else {
        render(CURATOR_EVIDENCE_TEMPLATE, &[("evidence", evidence)])
    };

    render(
        CURATOR_CONSOLIDATE_TEMPLATE,
        &[("entries", &entries_text), ("evidence_section", &evidence_section)],
    )
}

/// Deterministically execute consolidation operations (reject when the self-check fails, never pass by default).
///
/// Same principle as [`vet`]: the model only proposes, the program decides. It writes to disk (new entries / status changes / scope changes),
/// but does not rebuild the index (done uniformly by [`consolidate`]).
pub fn apply_consolidate_ops(
    store: &MemoryStore,
    ops: Vec<ConsolidateOp>,
    existing: &HashMap<String, Entry>,
    today: &str,
) -> Result<ConsolidateOutcome, CuratorError> {
    let mut working = existing.clone();
    let mut outcome = ConsolidateOutcome {
        empty: ops.is_empty(),
        ..Default::default()
    };

    for op in ops {
        match op {
            ConsolidateOp::Merge { mut target, sources } => {
                let target_id = target.id.clone();
                let sources: Vec<String> = sources
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if sources.is_empty() {
                    outcome.rejected.push(Rejected {
                        id: Some(target_id),
                        reason: "merge without sources".into(),
                    });
                    continue;
                }
                if let Some(missing) = sources.iter().find(|id| !working.contains_key(*id)) {
                    outcome.rejected.push(Rejected {
                        id: Some(target_id),
                        reason: format!("merge references unknown source `{missing}`"),
                    });
                    continue;
                }
                // Evidence inheritance: if the new entry gives no source, union the old entries' sources (real sources, not fabricated).
                if target.source.is_empty() {
                    target.source = union_sources(&sources, &working);
                }
                if target.supersedes.is_none() {
                    target.supersedes = Some(sources[0].clone());
                }
                match vet(target, &working, today, None, None) {
                    Ok(entry) => {
                        for id in &sources {
                            if let Some(old) = working.get(id).cloned()
                                && old.status != EntryStatus::Superseded
                            {
                                let mut old = old;
                                old.status = EntryStatus::Superseded;
                                store.write_entry(&old)?;
                                working.insert(id.clone(), old);
                                outcome.superseded += 1;
                            }
                        }
                        store.write_entry(&entry)?;
                        working.insert(entry.id.clone(), entry);
                        outcome.merged += 1;
                    }
                    Err(reason) => outcome.rejected.push(Rejected {
                        id: Some(target_id),
                        reason,
                    }),
                }
            }
            ConsolidateOp::Supersede { id, mut replacement } => {
                let id = id.trim().to_string();
                let Some(old) = working.get(&id).cloned() else {
                    outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason: "supersede target does not exist".into(),
                    });
                    continue;
                };
                if replacement.source.is_empty() {
                    replacement.source = old.source.clone();
                }
                replacement.supersedes = Some(id.clone());
                match vet(replacement, &working, today, None, None) {
                    Ok(entry) => {
                        if old.status != EntryStatus::Superseded {
                            let mut old = old;
                            old.status = EntryStatus::Superseded;
                            store.write_entry(&old)?;
                            working.insert(id.clone(), old);
                            outcome.superseded += 1;
                        }
                        store.write_entry(&entry)?;
                        working.insert(entry.id.clone(), entry);
                    }
                    Err(reason) => outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason,
                    }),
                }
            }
            ConsolidateOp::Requalify { id, scope, body } => {
                let id = id.trim().to_string();
                let Some(old) = working.get(&id).cloned() else {
                    outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason: "requalify target does not exist".into(),
                    });
                    continue;
                };
                if old.status == EntryStatus::Superseded {
                    outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason: "requalify a superseded entry".into(),
                    });
                    continue;
                }
                let mut entry = old;
                let mut changed = false;
                if let Some(scope) = scope {
                    let scope = scope.trim();
                    entry.scope = if scope.is_empty() {
                        None
                    } else {
                        Some(scope.to_string())
                    };
                    changed = true;
                }
                if let Some(body) = body {
                    let body = body.trim();
                    if body.is_empty() {
                        outcome.rejected.push(Rejected {
                            id: Some(id),
                            reason: "requalify with empty body".into(),
                        });
                        continue;
                    }
                    entry.body = body.to_string();
                    changed = true;
                }
                if !changed {
                    outcome.rejected.push(Rejected {
                        id: Some(id),
                        reason: "requalify with no changes".into(),
                    });
                    continue;
                }
                store.write_entry(&entry)?;
                working.insert(id, entry);
                outcome.requalified += 1;
            }
        }
    }
    Ok(outcome)
}

fn union_sources(ids: &[String], working: &HashMap<String, Entry>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if let Some(entry) = working.get(id) {
            for ref_ in &entry.source {
                if !out.contains(ref_) {
                    out.push(ref_.clone());
                }
            }
        }
    }
    out
}

/// Periodic consolidation (sleep learning): full scan -> model proposal -> deterministic self-check -> merge in -> rebuild index.
///
/// Returns empty directly when fewer than 2 entries (nothing to consolidate). When `sessions_dir` is given, it does the **check back against raw evidence**;
/// when it cannot (e.g. no session directory), it degrades to pure entry consolidation.
pub async fn consolidate(
    agent: &Agent,
    store: &MemoryStore,
    sessions_dir: Option<&Path>,
) -> Result<ConsolidateOutcome, CuratorError> {
    let pairs: Vec<(String, Entry)> = store
        .list_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|(path, entry)| (store.relative_path(&path), entry))
        .collect();
    if pairs.len() < 2 {
        return Ok(ConsolidateOutcome {
            empty: true,
            ..Default::default()
        });
    }
    let existing: HashMap<String, Entry> = pairs
        .iter()
        .map(|(_, entry)| (entry.id.clone(), entry.clone()))
        .collect();

    let evidence = match sessions_dir {
        Some(dir) => collect_evidence(&pairs, dir, MAX_EVIDENCE_CHARS),
        None => String::new(),
    };
    let prompt = build_consolidate_prompt(&pairs, &evidence);

    let raw = agent
        .complete(&prompt)
        .await
        .map_err(|err| CuratorError::Model(err.to_string()))?;
    let ops = parse_consolidate_ops(&raw)?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let outcome = apply_consolidate_ops(store, ops, &existing, &today)?;
    rebuild_index(store)?;
    rebuild_core(store)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str) -> Candidate {
        Candidate {
            id: id.into(),
            entry_type: EntryType::Preference,
            domain: Domain::Personal,
            subject: "rust-error-handling".into(),
            created_at: Some("2026-05-12".into()),
            valid_from: None,
            supersedes: None,
            confidence: Some(Confidence::High),
            scope: None,
            source: vec!["sessions/abc.jsonl#turn:2".into()],
            body: "Prefers thiserror for defining domain errors.".into(),
        }
    }

    #[test]
    fn parse_plain_and_fenced_json() {
        let plain = r#"[{"id":"a","type":"fact","subject":"s","body":"b","source":["x"]}]"#;
        assert_eq!(parse_candidates(plain).unwrap().len(), 1);
        let fenced = format!("blah\n```json\n{plain}\n```\ntrailing");
        assert_eq!(parse_candidates(&fenced).unwrap().len(), 1);
    }

    #[test]
    fn parse_empty_array() {
        assert!(parse_candidates("[]").unwrap().is_empty());
    }

    #[test]
    fn parse_rejects_non_json() {
        assert!(parse_candidates("no array here").is_err());
    }

    #[test]
    fn vet_accepts_valid_candidate() {
        let entry = vet(candidate("pref-rust"), &HashMap::new(), "2026-05-12", None, None).unwrap();
        assert_eq!(entry.id, "pref-rust");
        assert_eq!(entry.status, EntryStatus::Active);
        assert_eq!(entry.source, vec!["sessions/abc.jsonl#turn:2"]);
    }

    #[test]
    fn vet_rejects_bad_slug() {
        let mut c = candidate("Bad Id!");
        c.source = vec!["x".into()];
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_rejects_missing_source_without_session_ref() {
        let mut c = candidate("pref-x");
        c.source.clear();
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_stamps_session_ref_when_source_empty() {
        let mut c = candidate("pref-x");
        c.source.clear();
        let entry = vet(
            c,
            &HashMap::new(),
            "2026-05-12",
            Some("sessions/abc.jsonl"),
            None,
        )
        .unwrap();
        assert_eq!(entry.source, vec!["sessions/abc.jsonl"]);
    }

    #[test]
    fn vet_rejects_bad_date() {
        let mut c = candidate("pref-x");
        c.created_at = Some("2026/05/12".into());
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_rejects_dangling_supersedes() {
        let mut c = candidate("pref-new");
        c.supersedes = Some("pref-old".into());
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_accepts_existing_supersedes() {
        let old = vet(candidate("pref-old"), &HashMap::new(), "2026-05-12", None, None).unwrap();
        let mut existing = HashMap::new();
        existing.insert(old.id.clone(), old);
        let mut c = candidate("pref-new");
        c.supersedes = Some("pref-old".into());
        assert!(vet(c, &existing, "2026-05-12", None, None).is_ok());
    }

    #[test]
    fn vet_checks_source_file_exists() {
        let dir = std::env::temp_dir().join(format!("shirley_curator_src_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("s.jsonl"), "").unwrap();
        let mut c = candidate("pref-x");
        c.source = vec!["s.jsonl#turn:1".into()];
        assert!(vet(c.clone(), &HashMap::new(), "2026-05-12", None, Some(&dir)).is_ok());
        c.source = vec!["missing.jsonl#turn:1".into()];
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, Some(&dir)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_transcript_includes_user_and_tool() {
        let messages = vec![
            Message::System {
                content: "ignore me".into(),
            },
            Message::User {
                content: "hi".into(),
            },
            Message::Assistant {
                content: Some("here".into()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ];
        let text = render_transcript(&messages);
        assert!(text.contains("## User"));
        assert!(text.contains("hi"));
        assert!(!text.contains("ignore me"));
    }

    #[test]
    fn build_prompt_mentions_existing_and_boundary() {
        let existing = vec![(
            "preferences/a.md".into(),
            vet(candidate("pref-a"), &HashMap::new(), "2026-05-12", None, None).unwrap(),
        )];
        let prompt = build_prompt("## User\nhi\n", &existing, "sessions/x.jsonl");
        assert!(prompt.contains("partial evidence"));
        assert!(prompt.contains("pref-a"));
        assert!(prompt.contains("sessions/x.jsonl#turn:N"));
    }

    #[test]
    fn build_prompt_keeps_detail_in_body() {
        // Regression: body is no longer required to be compressed into one sentence -- first-line summary + blank line + concrete details,
        // otherwise the L2 "on-demand detail layer" would be empty and details could not be retrieved across sessions (travel-planning scenario).
        let prompt = build_prompt("## User\nhi\n", &[], "sessions/x.jsonl");
        assert!(prompt.contains("one-sentence summary"), "should require the first line to be a summary: {prompt}");
        assert!(prompt.contains("concrete details"), "should require the body to write concrete details: {prompt}");
        assert!(
            !prompt.contains("sum it up in one sentence"),
            "should no longer compress the body into one sentence: {prompt}"
        );
    }

    // ---- V2-B: periodic consolidation (consolidate) --------------------

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_curator_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write_entry(store: &MemoryStore, id: &str, status: EntryStatus) -> Entry {
        let mut entry = vet(candidate(id), &HashMap::new(), "2026-05-10", None, None).unwrap();
        entry.status = status;
        store.write_entry(&entry).unwrap();
        entry
    }

    #[test]
    fn parse_consolidate_ops_plain_and_fenced() {
        let plain = r#"[{"op":"requalify","id":"a","scope":"x"}]"#;
        assert_eq!(parse_consolidate_ops(plain).unwrap().len(), 1);
        let fenced = format!("blah\n```json\n{plain}\n```\ntail");
        assert_eq!(parse_consolidate_ops(&fenced).unwrap().len(), 1);
    }

    #[test]
    fn apply_merge_supersedes_sources_and_writes_target() {
        let root = tmpdir("merge");
        let store = MemoryStore::new(&root);
        let a = write_entry(&store, "a", EntryStatus::Active);
        let b = write_entry(&store, "b", EntryStatus::Active);
        let existing: HashMap<String, Entry> =
            [("a".to_string(), a), ("b".to_string(), b)].into_iter().collect();

        let mut target = candidate("ab-merged");
        target.source.clear(); // trigger evidence inheritance from sources
        let op = ConsolidateOp::Merge {
            target,
            sources: vec!["a".into(), "b".into()],
        };
        let out = apply_consolidate_ops(&store, vec![op], &existing, "2026-05-12").unwrap();
        assert_eq!(out.merged, 1);
        assert_eq!(out.superseded, 2);
        assert!(out.rejected.is_empty());

        let by_id: HashMap<String, Entry> = store
            .list_entries()
            .unwrap()
            .into_iter()
            .map(|(_, e)| (e.id.clone(), e))
            .collect();
        assert_eq!(by_id["a"].status, EntryStatus::Superseded);
        assert_eq!(by_id["b"].status, EntryStatus::Superseded);
        assert_eq!(by_id["ab-merged"].status, EntryStatus::Active);
        assert!(!by_id["ab-merged"].source.is_empty(), "evidence should be inherited from the old entries");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_supersede_marks_old_and_links_new() {
        let root = tmpdir("supersede");
        let store = MemoryStore::new(&root);
        let old = write_entry(&store, "old", EntryStatus::Active);
        let existing: HashMap<String, Entry> = [("old".to_string(), old)].into_iter().collect();

        let mut replacement = candidate("new");
        replacement.source.clear();
        let op = ConsolidateOp::Supersede {
            id: "old".into(),
            replacement,
        };
        let out = apply_consolidate_ops(&store, vec![op], &existing, "2026-05-12").unwrap();
        assert_eq!(out.superseded, 1);
        assert!(out.rejected.is_empty());

        let by_id: HashMap<String, Entry> = store
            .list_entries()
            .unwrap()
            .into_iter()
            .map(|(_, e)| (e.id.clone(), e))
            .collect();
        assert_eq!(by_id["old"].status, EntryStatus::Superseded);
        assert_eq!(by_id["new"].supersedes.as_deref(), Some("old"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_requalify_updates_scope_without_new_entry() {
        let root = tmpdir("requalify");
        let store = MemoryStore::new(&root);
        let entry = write_entry(&store, "topic", EntryStatus::Active);
        let existing: HashMap<String, Entry> = [("topic".to_string(), entry)].into_iter().collect();

        let op = ConsolidateOp::Requalify {
            id: "topic".into(),
            scope: Some("Linux only".into()),
            body: None,
        };
        let out = apply_consolidate_ops(&store, vec![op], &existing, "2026-05-12").unwrap();
        assert_eq!(out.requalified, 1);

        let by_id: HashMap<String, Entry> = store
            .list_entries()
            .unwrap()
            .into_iter()
            .map(|(_, e)| (e.id.clone(), e))
            .collect();
        assert_eq!(by_id["topic"].scope.as_deref(), Some("Linux only"));
        assert_eq!(by_id.len(), 1, "requalify should not create a new entry");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_rejects_merge_with_unknown_source() {
        let root = tmpdir("reject");
        let store = MemoryStore::new(&root);
        let existing: HashMap<String, Entry> = HashMap::new();
        let op = ConsolidateOp::Merge {
            target: candidate("m"),
            sources: vec!["ghost".into()],
        };
        let out = apply_consolidate_ops(&store, vec![op], &existing, "2026-05-12").unwrap();
        assert_eq!(out.merged, 0);
        assert_eq!(out.rejected.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_evidence_reads_turn_line_and_flags_missing() {
        let dir = tmpdir("evidence");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s.jsonl"), "line1\nline2-topic\nline3\n").unwrap();
        let mut e = vet(candidate("e"), &HashMap::new(), "2026-05-10", None, None).unwrap();
        e.source = vec!["s.jsonl#turn:2".into(), "missing.jsonl#turn:1".into()];
        let text = collect_evidence(&[("e.md".into(), e)], &dir, MAX_EVIDENCE_CHARS);
        assert!(text.contains("line2-topic"));
        assert!(text.contains("[missing evidence]"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_consolidate_prompt_lists_entries_and_evidence() {
        let entry = vet(candidate("pref-a"), &HashMap::new(), "2026-05-10", None, None).unwrap();
        let prompt = build_consolidate_prompt(&[("preferences/a.md".into(), entry)], "EVIDENCE");
        assert!(prompt.contains("pref-a"));
        assert!(prompt.contains("EVIDENCE"));
        assert!(prompt.contains("Never delete history"));
    }


    // ---- V2.5: deterministic rebuild of core.md -----------------------

    fn active_entry(id: &str, entry_type: EntryType, date: &str) -> Entry {
        let mut entry = vet(candidate(id), &HashMap::new(), date, None, None).unwrap();
        entry.entry_type = entry_type;
        entry.created_at = date.to_string();
        entry
    }

    #[test]
    fn rebuild_core_projects_active_entries_in_type_order() {
        let root = tmpdir("core_build");
        let store = MemoryStore::new(&root);
        store.write_entry(&active_entry("evt", EntryType::Event, "2026-05-12")).unwrap();
        store.write_entry(&active_entry("fact", EntryType::Fact, "2026-05-11")).unwrap();
        store.write_entry(&active_entry("pref", EntryType::Preference, "2026-05-10")).unwrap();

        rebuild_core(&store).unwrap();
        let core = store.read_core().expect("core should have been generated");
        // Type order: preference -> fact -> event (profile / preferences first).
        let p = core.find("[preference]").expect("should contain a preference");
        let f = core.find("[fact]").expect("should contain a fact");
        let e = core.find("[event]").expect("should contain an event");
        assert!(p < f && f < e, "core type order should be preference < fact < event");
        // One line per entry, containing the summary (first line of the body).
        assert!(core.contains("Prefers thiserror for defining domain errors."));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rebuild_core_skips_non_active_entries() {
        let root = tmpdir("core_skip");
        let store = MemoryStore::new(&root);
        store.write_entry(&active_entry("keep", EntryType::Fact, "2026-05-10")).unwrap();
        let mut gone = active_entry("gone", EntryType::Fact, "2026-05-10");
        gone.status = EntryStatus::Superseded;
        store.write_entry(&gone).unwrap();

        rebuild_core(&store).unwrap();
        let core = store.read_core().unwrap();
        assert!(core.contains("[fact]"));
        // superseded entries do not enter the resident layer (the only line should come from keep).
        assert_eq!(core.matches("- [").count(), 1, "only 1 active entry should enter core");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rebuild_core_removes_stale_core_when_no_active_entries() {
        let root = tmpdir("core_clear");
        let store = MemoryStore::new(&root);
        store.write_core("The user is Sean.").unwrap();
        assert!(store.read_core().is_some());

        // no active entries -> the old core.md should be deleted (avoid a stale profile being injected further).
        rebuild_core(&store).unwrap();
        assert!(store.read_core().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
