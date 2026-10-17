//! Subagent persistence (application layer, `docs/subagent.md` decision 9).
//!
//! A session's subagents are persisted **alongside** the session log, never inside it:
//!
//! ```text
//! ~/.shirley/workspaces/<workspace-id>/sessions/
//!   <session>.jsonl                 # the parent log -- still pure `Message`, untouched
//!   <session>.agents/
//!     index.jsonl                   # one JSON record per started/finished subagent
//!     <subagent-id>.jsonl           # the child's own transcript (a standard Message log)
//! ```
//!
//! **Why a sidecar**: the parent log must stay a pure `Message` stream (the three protocol
//! adapters' `encode_messages`, `MessageAdded`, storage and rendering all depend on that
//! invariant). Subagents are an application-layer concept, so they live next to the log, not in
//! it. The child transcript reuses [`JsonlSessionStore`] verbatim, so it can be read back by the
//! same code that restores a session.
//!
//! The store is deliberately **best-effort on the write side**: a persistence failure is reported
//! (so it is visible) but never aborts a running subagent -- losing the preview is far better than
//! losing the result.

use std::path::{Path, PathBuf};

use shirley_agent_sdk::{Message, Usage};

use crate::session::{JsonlSessionStore, SessionError, SessionStore};

/// One line of `.agents/index.jsonl`.
///
/// Tagged by `record`, so a reader can stream the file and tell started/finished apart without a
/// second file. `Usage` is embedded directly (it already derives serde).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum IndexRecord {
    /// Written the moment a subagent is dispatched.
    Started {
        id: String,
        kind: String,
        /// A short human-readable label for this subagent ("what is this agent doing"), so a
        /// transcript list is legible without reading the whole task. Model-supplied when given,
        /// otherwise derived from the first line of `task`.
        #[serde(default)]
        name: String,
        task: String,
        /// The owning session name (the "parent" in the tree).
        parent: Option<String>,
        started_ms: u64,
    },
    /// Written once, when the subagent reaches a terminal state.
    Finished {
        id: String,
        status: String,
        stop_reason: String,
        finished_ms: u64,
        summary: String,
        /// The child's own usage. **Kept separate** from the parent session's total
        /// (`docs/subagent.md` open question 5): the parent's context reading is not misled.
        usage: Usage,
    },
}

/// A session's `.agents/` directory: index + per-subagent transcripts.
#[derive(Debug, Clone)]
pub struct SubagentStore {
    /// `<storage_dir>/<session>.agents`
    dir: PathBuf,
    /// The owning session name (recorded as `parent` in the index).
    session: String,
}

impl SubagentStore {
    /// Build from a session-storage directory (`FileSessionCatalog::storage_dir`) and session name.
    pub fn for_session(storage_dir: &Path, session: &str) -> Self {
        Self {
            dir: storage_dir.join(format!("{session}.agents")),
            session: session.to_owned(),
        }
    }

    /// Build from an application state root (`<state>/sessions/<session>.agents`).
    pub fn new(root: &Path, session: &str) -> Self {
        Self::for_session(&root.join("sessions"), session)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn index_path(&self) -> PathBuf {
        self.dir.join("index.jsonl")
    }

    pub fn transcript_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    /// Append one record to the index (best-effort: the caller decides whether to surface an error).
    pub fn append_index(&self, record: &IndexRecord) -> Result<(), SessionError> {
        std::fs::create_dir_all(&self.dir)?;
        let line = serde_json::to_string(record)
            .map_err(|error| SessionError::Backend(error.to_string()))?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.index_path())?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Read the whole index (a missing file = an empty index, not an error).
    pub fn load_index(&self) -> Result<Vec<IndexRecord>, SessionError> {
        let text = match std::fs::read_to_string(self.index_path()) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut records = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let record = serde_json::from_str(line)
                .map_err(|error| SessionError::Backend(error.to_string()))?;
            records.push(record);
        }
        Ok(records)
    }

    /// Write the child's full transcript to `<id>.jsonl` (a standard `Message` log).
    ///
    /// The leading `System` is dropped: session logs never store the system prompt (it is
    /// regenerated on restore), and the child transcript follows the same convention.
    pub fn write_transcript(&self, id: &str, messages: &[Message]) -> Result<(), SessionError> {
        std::fs::create_dir_all(&self.dir)?;
        let store = JsonlSessionStore::open(self.transcript_path(id))?;
        for message in messages {
            if matches!(message, Message::System { .. }) {
                continue;
            }
            store.append(message)?;
        }
        Ok(())
    }

    /// Read a child transcript back (a missing file = empty; used by the preview / desktop).
    pub fn load_transcript(&self, id: &str) -> Result<Vec<Message>, SessionError> {
        let path = self.transcript_path(id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        JsonlSessionStore::open(path)?.load()
    }

    /// All subagent ids that have a transcript on disk (excluding the index).
    pub fn transcript_ids(&self) -> Vec<String> {
        let read = match std::fs::read_dir(&self.dir) {
            Ok(read) => read,
            Err(_) => return Vec::new(),
        };
        let mut ids: Vec<String> = read
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    return None;
                }
                let name = path.file_stem().and_then(|s| s.to_str())?;
                (name != "index").then(|| name.to_owned())
            })
            .collect();
        ids.sort();
        ids
    }
}
