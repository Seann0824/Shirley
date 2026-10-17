//! Application-side session storage (`docs/session.md`).
//!
//! Session persistence **belongs entirely to the application layer**: the contract ([`SessionStore`] / [`SessionError`]) and
//! the implementation both live here; the SDK no longer holds any session concept. The current on-disk format is **JSONL** -- one serialized
//! `Message` per line: append is a `write`, read-back is line-by-line `serde_json::from_str`. Why it was chosen:
//! zero dependencies, human-readable, hand-debuggable, and naturally append-only "log"-shaped, exactly aligned
//! with `docs/session.md` section 1 decision 4.
//!
//! What is persisted is the **full raw Message log**: no system (generated fresh on restore).
//! `ContextSummary` is a first-class message in the log; compaction goes through `append` too.
//!
//! Persistence timing is driven by the application layer: when `Session::apply_event` receives the SDK's `MessageAdded` event it
//! calls [`SessionStore::append`] (the SDK only emits events, never persists).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use shirley_agent_sdk::Message;

/// Storage abstraction for session logs.
///
/// The three methods map to three operations, **exactly the full semantics of session writing**: append (normal turns + compaction),
/// read-back (restore), truncate-tail (rewind). Deliberately no `delete` / `update` / `search` -- not needed.
///
/// **The signature is synchronous**: `SessionManager`'s restore / truncate happen on synchronous paths, and a synchronous interface spares
/// callers from going async. If a storage backend is async-driven, the implementation absorbs it internally. Synchronous contract, free implementation.
pub trait SessionStore: Send + Sync {
    /// Append a message to the tail of the log. The `ContextSummary` produced by compaction also goes through here.
    fn append(&self, message: &Message) -> Result<(), SessionError>;

    /// Read the full log (read-only, in order).
    fn load(&self) -> Result<Vec<Message>, SessionError>;

    /// Truncate to the first `len` entries (for rewind; tail-only, never producing interior holes).
    ///
    /// Does nothing when `len` is not smaller than the current length (idempotent, avoids out-of-bounds).
    fn truncate(&self, len: usize) -> Result<(), SessionError>;
}

/// Session storage failure.
///
/// Display format is uniformly `[prefix]: details`, consistent with the SDK's layered errors (`AdapterError` / `ToolError` /
/// `SandboxError` / `WorkspaceError`); the prefix stays next to the variant.
///
/// Persistence failure is currently always **treated as an error** (`docs/session.md` section 2.2): continuing after a persistence failure
/// amounts to pretending there is a saved state, which is more dangerous than erroring out.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Underlying storage I/O failure (write, read, serialization, etc.).
    #[error("[session storage failure]: {0}")]
    Io(#[from] std::io::Error),

    /// An error reported by the storage backend itself (e.g. a SQL failure), with free-text details.
    #[error("[session storage error]: {0}")]
    Backend(String),
}

/// JSONL session log: one `Message` per line.
///
/// Internally holds a lock protecting the file handle and the atomicity of truncate/append -- `SessionStore`'s methods take
/// `&self` (synchronous signature, see `docs/session.md` section 2.1), so mutable state can only hide inside the lock.
pub struct JsonlSessionStore {
    path: PathBuf,
    /// Handle used for appending; `truncate` reopens it. The lock keeps the two operations from interleaving.
    file: Mutex<File>,
}

impl JsonlSessionStore {
    /// Open (creating if absent) the JSONL session log at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// Read the full log (line-by-line deserialization). Blank lines are skipped.
    fn read_all(&self) -> Result<Vec<Message>, SessionError> {
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut log = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let message: Message = serde_json::from_str(&line)
                .map_err(|e| SessionError::Backend(format!("failed to parse session log line {}: {e}", log.len() + 1)))?;
            log.push(message);
        }
        Ok(log)
    }

    /// Rewrite the file wholesale with the given log (used by `truncate`). Writes a temp file then atomically replaces.
    ///
    /// **The caller must already hold `self.file`'s lock** -- this does not lock again, otherwise it would
    /// deadlock by nesting the same non-reentrant lock as the caller. The function replaces the file and writes the new append handle back.
    fn rewrite_locked(&self, guard: &mut File, log: &[Message]) -> Result<(), SessionError> {
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut file = File::create(&tmp)?;
            for message in log {
                let line = serde_json::to_string(message)
                    .map_err(|e| SessionError::Backend(format!("failed to serialize session message: {e}")))?;
                writeln!(file, "{line}")?;
            }
            file.flush()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        // Reopen the append handle, pointing at the replaced file, and write it back to the caller's guard.
        *guard = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&self.path)?;
        Ok(())
    }
}

impl SessionStore for JsonlSessionStore {
    fn append(&self, message: &Message) -> Result<(), SessionError> {
        let line = serde_json::to_string(message)
            .map_err(|e| SessionError::Backend(format!("failed to serialize session message: {e}")))?;
        let mut file = self.file.lock().expect("session lock poisoned");
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }

    fn load(&self) -> Result<Vec<Message>, SessionError> {
        // Mutually exclusive with append / truncate, avoiding reads of half-written lines.
        let _guard = self.file.lock().expect("session lock poisoned");
        self.read_all()
    }

    fn truncate(&self, len: usize) -> Result<(), SessionError> {
        let mut guard = self.file.lock().expect("session lock poisoned");
        let mut log = self.read_all()?;
        if len < log.len() {
            log.truncate(len);
            self.rewrite_locked(&mut guard, &log)?;
        }
        Ok(())
    }
}

/// Lazy session log: **does not touch disk at startup**; the file is created only on the first `append` (i.e. the first message).
///
/// Motivation: Codex's UX -- opening the app does not create an empty session file; only actually sending a message
/// (producing an interaction) creates a session. This keeps the session list from being clogged with a pile of "opened but never chatted" empty files.
///
/// Semantically it is just a [`SessionStore`]: `Session` holds it as usual, and the send path need not be aware of
/// "delayed creation" at all. There are only two differences:
/// - the name is decided at **construction time** (for the UI footer / current-session marker), but **no file is created**;
/// - it is **lazily materialized** only on the first `append`: writes the JSONL under the decided name (and the title sidecar).
///
/// Before materialization, `load` / `truncate` are treated as an empty log (no file, no content);
/// after materialization it permanently holds an internal [`JsonlSessionStore`], behaving exactly like a directly opened session.
///
/// If a title is given, it is written as the sidecar `<name>.title` on materialization (same semantics as
/// [`FileSessionCatalog::create_named`]).
pub struct LazySessionStore {
    catalog: Arc<FileSessionCatalog>,
    /// The name decided at startup (`list` cannot find it before materialization, since there is no file).
    name: String,
    /// The underlying store after materialization (created on the first `append`).
    inner: Mutex<Option<Arc<JsonlSessionStore>>>,
    /// Optional title (written as a sidecar on materialization).
    title: Option<String>,
}

impl LazySessionStore {
    fn new(catalog: Arc<FileSessionCatalog>, name: String, title: Option<String>) -> Self {
        Self {
            catalog,
            name,
            inner: Mutex::new(None),
            title,
        }
    }

    /// Materialize: write the empty log under the decided name, write the title sidecar, return the underlying store. Idempotent.
    fn materialize(&self) -> Result<Arc<JsonlSessionStore>, SessionError> {
        let mut inner = self.inner.lock().expect("session lock poisoned");
        if let Some(store) = inner.as_ref() {
            return Ok(store.clone());
        }
        let store = Arc::new(JsonlSessionStore::open(self.catalog.path_for(&self.name))?);
        if let Some(title) = self.title.as_deref() {
            self.catalog.write_title(&self.name, title)?;
        }
        *inner = Some(store.clone());
        Ok(store)
    }
}

impl SessionStore for LazySessionStore {
    fn append(&self, message: &Message) -> Result<(), SessionError> {
        // The first append triggers materialization; afterward it just delegates.
        self.materialize()?.append(message)
    }

    fn load(&self) -> Result<Vec<Message>, SessionError> {
        // Before materialization = empty log (no file, no content).
        match self.inner.lock().expect("session lock poisoned").as_ref() {
            Some(store) => store.load(),
            None => Ok(Vec::new()),
        }
    }

    fn truncate(&self, len: usize) -> Result<(), SessionError> {
        match self.inner.lock().expect("session lock poisoned").as_ref() {
            Some(store) => store.truncate(len),
            // No log before materialization, so truncation is a no-op (idempotent, consistent with the contract).
            None => Ok(()),
        }
    }
}

/// A selectable session (for `/session` switching).
///
/// `name` is the unique identity (the file stem); `label` is for humans (a custom title wins, otherwise it falls back to
/// `name`); `preview` is a summary of the session's first user message -- if the list showed only timestamps
/// they would be indistinguishable, and the preview lets the user recognize the session.
///
/// `modified_ms` / `turns` are metadata for the UI (Codex's resume picker shows
/// time and turn count); they do not participate in identity or lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub name: String,
    pub label: String,
    pub preview: String,
    /// Last-modified time (Unix millis; 0 = unknown). The list sorts by it descending.
    pub modified_ms: u64,
    /// Number of user turns (count of `User` messages in the session).
    pub turns: usize,
}

impl SessionEntry {
    /// Whether the session has content (decides whether the UI shows a preview).
    pub fn is_empty(&self) -> bool {
        self.preview.is_empty()
    }
}

/// Session catalog (application layer): list / open / create / rename / delete sessions.
///
/// Symmetric with `models::ModelCatalog` -- it converges "which sessions are currently selectable and how they are managed" behind
/// an interface. The current implementation scans a local directory ([`FileSessionCatalog`]); if this is later swapped for a remote /
/// database backend, neither the commands nor the picker UI need to change.
///
/// **The TUI and desktop share the same implementation**: both sides' session list / switch / rename / delete
/// land in this layer, so they naturally share the same `<config_dir>/shirley/workspaces/<workspace-id>/sessions` data.
pub trait SessionCatalog: Send + Sync {
    /// List all sessions, **sorted by last-modified time descending** (newest first).
    fn list(&self) -> Result<Vec<SessionEntry>, SessionError>;

    /// Open the store for the named session (creating an empty file if absent).
    fn open(&self, name: &str) -> Result<Arc<dyn SessionStore>, SessionError>;

    /// Create a new empty session; return its entry and store.
    fn create(&self) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError>;

    /// Lazily create a session: **no disk write yet**; the returned store creates the file only on the first `append`.
    ///
    /// Used for "startup default empty session" -- a session with no chat should not leave an empty file in the list.
    /// The returned [`SessionEntry`] has empty `preview` / `turns` and a placeholder `name`
    /// (a temporary name before materialization; the UI must not look up sessions by it).
    ///
    /// The default implementation degrades to [`SessionCatalog::create`] (immediate disk write) -- backends without lazy
    /// support need no change, they just miss out on "delayed creation".
    fn create_lazy(
        &self,
        _title: Option<&str>,
    ) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        self.create()
    }

    /// Create a session with an optional title (`None` = anonymous; the UI falls back to the first user message / name).
    ///
    /// The default implementation ignores the title and degrades to [`SessionCatalog::create`] -- backends without title
    /// support need no change.
    fn create_named(
        &self,
        _title: Option<&str>,
    ) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        self.create()
    }

    /// Rename a session: **changes only the title, not the session identity / content** (Codex's `/rename` semantics,
    /// it does not touch the transcript).
    ///
    /// The default implementation errors -- backends without metadata support (e.g. [`EmptySessionCatalog`]) need not implement it.
    fn rename(&self, _name: &str, _title: &str) -> Result<(), SessionError> {
        Err(SessionError::Backend("this session catalog does not support renaming".into()))
    }

    /// Delete a session (along with its log and title). The default implementation errors.
    fn delete(&self, _name: &str) -> Result<(), SessionError> {
        Err(SessionError::Backend("this session catalog does not support deletion".into()))
    }

    /// The directory holding the session logs (for cross-directory moves). The default returns `None` -- backends without
    /// directory semantics (e.g. [`EmptySessionCatalog`]) need not implement it.
    fn storage_dir(&self) -> Option<PathBuf> {
        None
    }

    /// Move a session (along with its title) from the current directory to `dest_dir`.
    ///
    /// Used for "move a session to another project" -- each project has its own
    /// `<config_dir>/shirley/workspaces/<workspace-id>/sessions` directory, so a move is copying `<name>.jsonl` and
    /// `<name>.title` into the target directory. If the target already has a same-named session it **errors**
    /// (no silent overwrite, to avoid data loss). The default implementation errors.
    fn move_to(&self, _name: &str, _dest_dir: &Path) -> Result<(), SessionError> {
        Err(SessionError::Backend("this session catalog does not support moving".into()))
    }
}

/// A session catalog under a globally stored workspace state directory.
///
/// Each session is an independent JSONL log (reusing [`JsonlSessionStore`]),
/// and scanning the directory is "listing sessions". The name defaults to a timestamp, naturally ordered and needing no extra index.
///
/// The title is stored separately as the sidecar `<name>.title` (plain text): the log stores only `Message`
/// (`docs/session.md` section 1 decision 1); stuffing the title into the message stream would break this invariant.
pub struct FileSessionCatalog {
    dir: PathBuf,
}

/// The version stamped into `index.json`; a mismatch is treated as an empty cache (full recompute).
const INDEX_VERSION: u32 = 1;

/// A single cached session summary inside `index.json`.
///
/// Mirrors the `SessionEntry` fields that are expensive to recompute (reading the whole `.jsonl`
/// to derive `preview` / `turns`). `label` is **not** cached-authoritative: it is re-read from the
/// tiny `.title` sidecar every `list()` so `rename` takes effect immediately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    preview: String,
    #[serde(default)]
    modified_ms: u64,
    #[serde(default)]
    turns: usize,
}

/// On-disk `index.json`: a best-effort cache of session summaries, keyed by name on load.
#[derive(Debug, Serialize, Deserialize)]
struct SessionIndex {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    sessions: Vec<CachedEntry>,
}

impl FileSessionCatalog {
    /// `root` is an application state directory, not the source workspace.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            dir: root.as_ref().join("sessions"),
        }
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.jsonl"))
    }

    /// Title sidecar path: `<name>.title` (plain text).
    fn title_path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.title"))
    }

    /// Read the session title (returns `None` when unnamed).
    fn read_title(&self, name: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.title_path_for(name)).ok()?;
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }

    /// Write the session title: an empty string means "clear the title" (delete the sidecar); non-empty is written.
    fn write_title(&self, name: &str, title: &str) -> Result<(), SessionError> {
        std::fs::create_dir_all(&self.dir)?;
        let title = title.trim();
        if title.is_empty() {
            let _ = std::fs::remove_file(self.title_path_for(name));
        } else {
            std::fs::write(self.title_path_for(name), title)?;
        }
        Ok(())
    }

    /// Open the session with the given name; `JsonlSessionStore::open` creates the file if it is absent.
    fn open_named(&self, name: &str) -> Result<Arc<dyn SessionStore>, SessionError> {
        let store = JsonlSessionStore::open(self.path_for(name))?;
        Ok(Arc::new(store))
    }

    /// Generate a non-colliding new session name (timestamp, with a suffix appended if needed).
    fn fresh_name(&self) -> String {
        let base = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        if !self.path_for(&base).exists() {
            return base;
        }
        for suffix in 2..1000 {
            let candidate = format!("{base}-{suffix}");
            if !self.path_for(&candidate).exists() {
                return candidate;
            }
        }
        base
    }

    /// Path to the cache file: `<dir>/index.json` (same directory as the `*.jsonl` / `*.title`).
    fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    /// Load the cache as a `name -> CachedEntry` map. Missing / unreadable / wrong-version /
    /// malformed all degrade to an **empty map** (full recompute) -- a broken cache must never
    /// make `list()` fail.
    fn load_index(&self) -> HashMap<String, CachedEntry> {
        let Ok(text) = std::fs::read_to_string(self.index_path()) else {
            return HashMap::new();
        };
        let Ok(index) = serde_json::from_str::<SessionIndex>(&text) else {
            return HashMap::new();
        };
        if index.version != INDEX_VERSION {
            return HashMap::new();
        }
        index
            .sessions
            .into_iter()
            .map(|entry| (entry.name.clone(), entry))
            .collect()
    }

    /// Best-effort atomic write of the cache: write a unique temp file then rename over
    /// `index.json`. Any failure is swallowed (the cache is only an optimization).
    fn write_index(&self, sessions: &[CachedEntry]) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let index = SessionIndex {
            version: INDEX_VERSION,
            sessions: sessions.to_vec(),
        };
        let Ok(json) = serde_json::to_string(&index) else {
            return;
        };
        // A per-call unique suffix avoids concurrent writers clobbering each other's temp file.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = self.dir.join(format!("index.json.{}.{}.tmp", std::process::id(), unique));
        if std::fs::write(&tmp, json).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        if std::fs::rename(&tmp, self.index_path()).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Adopt the old single-file log: if `<state>/session.jsonl` exists and there is currently
    /// no session yet, adopt it as the first session. Called only once at startup.
    ///
    /// This way upgrading to the multi-session layout does not lose the user's existing conversation (the old version had only one log).
    pub fn adopt_legacy(&self) -> Result<(), SessionError> {
        let legacy = self.dir.parent().map(|p| p.join("session.jsonl"));
        let Some(legacy) = legacy else {
            return Ok(());
        };
        if !legacy.exists() {
            return Ok(());
        }
        // If there are already sessions, do not migrate again, to avoid duplicates.
        if !self.list()?.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        std::fs::rename(&legacy, self.path_for("legacy"))?;
        Ok(())
    }
}

impl SessionCatalog for FileSessionCatalog {
    fn list(&self) -> Result<Vec<SessionEntry>, SessionError> {
        // Old cache: `name -> CachedEntry`. Used to reuse `preview` / `turns` for unchanged files.
        let cached = self.load_index();
        let mut entries: Vec<(u64, SessionEntry)> = Vec::new();
        // The freshly reconciled cache we will (maybe) write back.
        let mut next_cache: Vec<CachedEntry> = Vec::new();

        let read = match std::fs::read_dir(&self.dir) {
            Ok(read) => read,
            // The directory does not exist yet = no sessions yet, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        for item in read {
            let item = item?;
            let path = item.path();
            // `index.json` is not `.jsonl`, so this extension filter skips it naturally.
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let modified_ms = item
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            // The title is always re-read from the tiny `.title` sidecar, so `rename` (which only
            // touches `.title`, not the `.jsonl` mtime) takes effect immediately with no hook.
            let label = self.read_title(name).unwrap_or_else(|| name.to_owned());
            // Reuse the cached summary only when the file's mtime is unchanged; otherwise recompute
            // by reading the whole `.jsonl`.
            let (preview, turns) = match cached.get(name) {
                Some(entry) if entry.modified_ms == modified_ms => {
                    (entry.preview.clone(), entry.turns)
                }
                _ => summary_of(&path),
            };
            next_cache.push(CachedEntry {
                name: name.to_owned(),
                label: label.clone(),
                preview: preview.clone(),
                modified_ms,
                turns,
            });
            entries.push((
                modified_ms,
                SessionEntry {
                    name: name.to_owned(),
                    label,
                    preview,
                    modified_ms,
                    turns,
                },
            ));
        }

        // Compare the reconciled cache with the old one; only write back when something changed
        // (name set, or any name's modified_ms / preview / turns / label differs).
        if index_changed(&cached, &next_cache) {
            self.write_index(&next_cache);
        }

        // Most recently modified first.
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(entries.into_iter().map(|(_, e)| e).collect())
    }

    fn open(&self, name: &str) -> Result<Arc<dyn SessionStore>, SessionError> {
        self.open_named(name)
    }

    fn create(&self) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        self.create_named(None)
    }

    fn create_lazy(
        &self,
        title: Option<&str>,
    ) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        // The name is decided now (for the UI to display); the file waits until the first append.
        let catalog = Arc::new(FileSessionCatalog {
            dir: self.dir.clone(),
        });
        let name = self.fresh_name();
        let store = Arc::new(LazySessionStore::new(
            catalog,
            name.clone(),
            title.map(str::to_owned),
        ));
        let entry = SessionEntry {
            label: title.unwrap_or(name.as_str()).to_owned(),
            name,
            preview: String::new(),
            modified_ms: 0,
            turns: 0,
        };
        Ok((entry, store))
    }

    fn create_named(
        &self,
        title: Option<&str>,
    ) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        let name = self.fresh_name();
        let store = self.open_named(&name)?;
        if let Some(title) = title {
            self.write_title(&name, title)?;
        }
        let entry = SessionEntry {
            label: self.read_title(&name).unwrap_or_else(|| name.clone()),
            name,
            preview: String::new(),
            modified_ms: 0,
            turns: 0,
        };
        Ok((entry, store))
    }

    fn rename(&self, name: &str, title: &str) -> Result<(), SessionError> {
        if !self.path_for(name).exists() {
            return Err(SessionError::Backend(format!("session does not exist: {name}")));
        }
        self.write_title(name, title)
    }

    fn delete(&self, name: &str) -> Result<(), SessionError> {
        let log = self.path_for(name);
        if !log.exists() {
            return Err(SessionError::Backend(format!("session does not exist: {name}")));
        }
        std::fs::remove_file(&log)?;
        // The title sidecar is subordinate to the log: without the log it is meaningless, so delete it too (best-effort).
        let _ = std::fs::remove_file(self.title_path_for(name));
        Ok(())
    }

    fn storage_dir(&self) -> Option<PathBuf> {
        Some(self.dir.clone())
    }

    fn move_to(&self, name: &str, dest_dir: &Path) -> Result<(), SessionError> {
        let log = self.path_for(name);
        if !log.exists() {
            return Err(SessionError::Backend(format!("session does not exist: {name}")));
        }
        let dest_log = dest_dir.join(format!("{name}.jsonl"));
        if dest_log.exists() {
            return Err(SessionError::Backend(format!("the target project already has a session with this name: {name}")));
        }
        std::fs::create_dir_all(dest_dir)?;
        // Move the log first (source of truth), then the title sidecar (subordinate; missing is not an error).
        std::fs::rename(&log, &dest_log)?;
        let title = self.title_path_for(name);
        if title.exists() {
            let _ = std::fs::rename(&title, dest_dir.join(format!("{name}.title")));
        }
        Ok(())
    }
}

/// Whether the reconciled cache differs from the previously loaded one, so `list()` only writes
/// `index.json` when something actually changed. Compares name sets and, for each name, the
/// `modified_ms` / `preview` / `turns` / `label` fields.
fn index_changed(old: &HashMap<String, CachedEntry>, next: &[CachedEntry]) -> bool {
    if old.len() != next.len() {
        return true;
    }
    for entry in next {
        match old.get(&entry.name) {
            Some(prev)
                if prev.modified_ms == entry.modified_ms
                    && prev.preview == entry.preview
                    && prev.turns == entry.turns
                    && prev.label == entry.label => {}
            _ => return true,
        }
    }
    false
}

/// Read a session's preview (first user message, truncated to 40 chars) and user turn count. Returns `("", 0)` on failure.
fn summary_of(path: &Path) -> (String, usize) {
    let Ok(file) = File::open(path) else {
        return (String::new(), 0);
    };
    let reader = BufReader::new(file);
    let mut preview = String::new();
    let mut turns = 0usize;
    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(Message::User { content }) = serde_json::from_str::<Message>(&line) {
            turns += 1;
            if preview.is_empty() {
                let one_line = content.as_text().replace('\n', " ");
                preview = truncate_chars(&one_line, 40);
            }
        }
    }
    (preview, turns)
}

/// Truncate by character count (CJK counted per char, avoiding splitting UTF-8 by bytes).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// Take the body of the **first user message** in a session (as input for auto-titling).
///
/// Iteration order is message order; `ContextSummary` / assistant / tool messages are skipped. When there is no user
/// message it returns `None`. The body is truncated by character ([`AUTO_TITLE_INPUT_CHARS`]), to avoid feeding the whole
/// long input to the titling model.
pub fn first_user_message(messages: &[Message]) -> Option<String> {
    messages.iter().find_map(|message| match message {
        Message::User { content } => Some(truncate_chars(&content.as_text(), AUTO_TITLE_INPUT_CHARS)),
        _ => None,
    })
}

/// The one-shot completion prompt for auto-titling: have the model summarize the first user message into a short title.
///
/// Output only the title itself -- cleaning is left to [`sanitize_title`] (the model occasionally adds quotes / prefixes).
pub fn title_prompt(first_user_message: &str) -> String {
    format!(
        "Read the user's first message and summarize the topic of this session in no more than 20 characters, as the session title.\
Output only the title itself: no quotes, no punctuation, no explanation, no newlines.\n\nThe user's first message:\n{first_user_message}"
    )
}

/// Clean the title returned by the model: take the first non-empty line, strip wrapping quotes / backticks and surrounding whitespace,
/// collapse internal whitespace, truncate to [`AUTO_TITLE_MAX_CHARS`]. Returns `None` if empty after cleaning.
pub fn sanitize_title(raw: &str) -> Option<String> {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let trimmed = line
        .trim_matches(|ch| matches!(ch, '"' | '\'' | '`' | '“' | '”' | '‘' | '’'))
        .trim();
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(truncate_chars(&collapsed, AUTO_TITLE_MAX_CHARS))
}

/// Auto-title input truncation (character count).
const AUTO_TITLE_INPUT_CHARS: usize = 500;
/// Auto-title output truncation (character count) -- same order of magnitude as `summary_of`'s preview length.
const AUTO_TITLE_MAX_CHARS: usize = 40;

/// Empty session catalog: the default for `App::new`, for tests and default construction that do not care about session switching.
/// Empty session catalog: the default for `App::new`, for tests and default construction that do not care about session switching.
///
/// Lists as empty; open / create both error -- no disk writes, no side effects.
#[allow(dead_code)]
pub struct EmptySessionCatalog;

impl SessionCatalog for EmptySessionCatalog {
    fn list(&self) -> Result<Vec<SessionEntry>, SessionError> {
        Ok(Vec::new())
    }

    fn open(&self, _name: &str) -> Result<Arc<dyn SessionStore>, SessionError> {
        Err(SessionError::Backend("the empty session catalog does not support opening sessions".into()))
    }

    fn create(&self) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        Err(SessionError::Backend("the empty session catalog does not support creating sessions".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shirley_session_{tag}_{}.jsonl", std::process::id()))
    }

    fn user(content: &str) -> Message {
        Message::User {
            content: content.to_owned().into(),
        }
    }

    #[test]
    fn append_load_round_trip_persists_across_handles() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        {
            let store = JsonlSessionStore::open(&path).unwrap();
            store.append(&user("a")).unwrap();
            store.append(&user("b")).unwrap();
        }
        // Read back via a new handle -- simulating a process restart.
        let store = JsonlSessionStore::open(&path).unwrap();
        let log = store.load().unwrap();
        assert_eq!(log.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn context_summary_round_trips() {
        let path = tmp_path("summary");
        let _ = std::fs::remove_file(&path);
        let store = JsonlSessionStore::open(&path).unwrap();
        store
            .append(&Message::ContextSummary {
                content: "<current_goal>x</current_goal>".to_owned(),
            })
            .unwrap();
        match &store.load().unwrap()[0] {
            Message::ContextSummary { content } => assert!(content.contains("current_goal")),
            other => panic!("expected ContextSummary, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn truncate_rewrites_and_persists() {
        let path = tmp_path("truncate");
        let _ = std::fs::remove_file(&path);
        let store = JsonlSessionStore::open(&path).unwrap();
        for c in ["a", "b", "c", "d"] {
            store.append(&user(c)).unwrap();
        }
        store.truncate(2).unwrap();
        // Reopen the handle to confirm it persisted.
        let reopened = JsonlSessionStore::open(&path).unwrap();
        assert_eq!(reopened.load().unwrap().len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn truncate_is_idempotent_when_not_smaller() {
        let path = tmp_path("idem");
        let _ = std::fs::remove_file(&path);
        let store = JsonlSessionStore::open(&path).unwrap();
        store.append(&user("a")).unwrap();
        store.truncate(5).unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_after_truncate_works() {
        let path = tmp_path("append_after_trunc");
        let _ = std::fs::remove_file(&path);
        let store = JsonlSessionStore::open(&path).unwrap();
        for c in ["a", "b", "c"] {
            store.append(&user(c)).unwrap();
        }
        store.truncate(1).unwrap();
        // Appending still works after truncation (the handle was reopened).
        store.append(&user("d")).unwrap();
        let log = store.load().unwrap();
        assert_eq!(log.len(), 2);
        let _ = std::fs::remove_file(&path);
    }
    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_catalog_{tag}_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn catalog_create_then_list_shows_session() {
        let dir = tmp_dir("create");
        let catalog = FileSessionCatalog::new(&dir);
        assert!(catalog.list().unwrap().is_empty());
        let (entry, store) = catalog.create().unwrap();
        store.append(&user("hello")).unwrap();
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, entry.name);
        assert_eq!(listed[0].preview, "hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_open_returns_isolated_store() {
        let dir = tmp_dir("open");
        let catalog = FileSessionCatalog::new(&dir);
        let (a, sa) = catalog.create().unwrap();
        sa.append(&user("a-msg")).unwrap();
        let (_b, sb) = catalog.create().unwrap();
        sb.append(&user("b-msg")).unwrap();
        // Open a: only a's content is visible, b's content does not leak.
        let reopened = catalog.open(&a.name).unwrap();
        let log = reopened.load().unwrap();
        assert_eq!(log.len(), 1);
        assert!(matches!(&log[0], Message::User { content } if content == "a-msg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_preview_truncates_long_first_user_message() {
        let dir = tmp_dir("preview");
        let catalog = FileSessionCatalog::new(&dir);
        let (_e, store) = catalog.create().unwrap();
        let long = "字".repeat(80);
        store.append(&user(&long)).unwrap();
        let preview = &catalog.list().unwrap()[0].preview;
        // 40 characters + an ellipsis.
        assert_eq!(preview.chars().count(), 41);
        assert!(preview.ends_with('…'));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_adopts_legacy_single_file() {
        let dir = tmp_dir("legacy");
        let shirley = dir.clone();
        std::fs::create_dir_all(&shirley).unwrap();
        let legacy = shirley.join("session.jsonl");
        std::fs::write(
            &legacy,
            format!("{}\n", serde_json::to_string(&user("old conversation")).unwrap()),
        )
        .unwrap();
        let catalog = FileSessionCatalog::new(&dir);
        catalog.adopt_legacy().unwrap();
        // The old file is moved into the sessions directory and can still be read back.
        assert!(!legacy.exists());
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "legacy");
        assert_eq!(listed[0].preview, "old conversation");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_create_named_sets_title_and_counts_turns() {
        let dir = tmp_dir("named");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_named(Some("migration task")).unwrap();
        assert_eq!(entry.label, "migration task");
        store.append(&user("first turn")).unwrap();
        store.append(&Message::Assistant {
            content: Some("reply".into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
        })
        .unwrap();
        store.append(&user("second turn")).unwrap();
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        // The title is persisted in the sidecar; the name is still a timestamp (identity unchanged).
        assert_eq!(listed[0].label, "migration task");
        assert_eq!(listed[0].name, entry.name);
        assert_eq!(listed[0].preview, "first turn");
        assert_eq!(listed[0].turns, 2);
        assert!(listed[0].modified_ms > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_rename_changes_label_not_identity_or_content() {
        let dir = tmp_dir("rename");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create().unwrap();
        store.append(&user("content")).unwrap();
        catalog.rename(&entry.name, "new name").unwrap();
        let listed = catalog.list().unwrap();
        assert_eq!(listed[0].label, "new name");
        // Identity and content unchanged (rename does not touch the transcript).
        assert_eq!(listed[0].name, entry.name);
        assert_eq!(listed[0].preview, "content");
        // Reopening still reads the same log.
        assert_eq!(catalog.open(&entry.name).unwrap().load().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_delete_removes_log_and_title() {
        let dir = tmp_dir("delete");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_named(Some("to delete")).unwrap();
        store.append(&user("x")).unwrap();
        assert_eq!(catalog.list().unwrap().len(), 1);
        catalog.delete(&entry.name).unwrap();
        assert!(catalog.list().unwrap().is_empty());
        // Deleting again errors (already gone).
        assert!(catalog.delete(&entry.name).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_writes_index_json_on_first_scan() {
        let dir = tmp_dir("index_write");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_named(Some("first title")).unwrap();
        store.append(&user("hello world")).unwrap();
        // First list() performs a full scan and writes the cache.
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        let index_path = catalog.index_path();
        assert!(index_path.exists(), "index.json should be written on first list");
        let raw = std::fs::read_to_string(&index_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["version"], 1);
        let sessions = parsed["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["name"], entry.name);
        assert_eq!(sessions[0]["label"], "first title");
        assert_eq!(sessions[0]["preview"], "hello world");
        assert_eq!(sessions[0]["turns"], 1);
        assert!(sessions[0]["modified_ms"].as_u64().unwrap() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_reconciles_index_by_mtime_and_drops_deleted_sessions() {
        let dir = tmp_dir("index_reconcile");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create().unwrap();
        store.append(&user("first message")).unwrap();
        // First list builds the cache.
        let first = catalog.list().unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].preview, "first message");
        assert_eq!(first[0].turns, 1);
        assert!(catalog.index_path().exists());

        // Second list with no changes hits the cache and returns the same result.
        let second = catalog.list().unwrap();
        assert_eq!(second, first);

        // Rename only touches the tiny .title sidecar (jsonl mtime unchanged): label must update
        // immediately even though preview/turns come from the cache.
        catalog.rename(&entry.name, "renamed").unwrap();
        let renamed = catalog.list().unwrap();
        assert_eq!(renamed[0].label, "renamed");
        assert_eq!(renamed[0].preview, "first message");
        assert_eq!(renamed[0].turns, 1);

        // Mutating the jsonl (new user turn) changes its mtime -> preview/turns recomputed.
        // Bump the mtime explicitly to avoid same-millisecond flakiness on coarse filesystems.
        let log = catalog.path_for(&entry.name);
        let mut f = OpenOptions::new().append(true).open(&log).unwrap();
        writeln!(f, "{}", serde_json::to_string(&user("second message")).unwrap()).unwrap();
        drop(f);
        let bumped = std::time::SystemTime::now() + std::time::Duration::from_millis(10);
        let _ = filetime_set(&log, bumped);
        let after_append = catalog.list().unwrap();
        assert_eq!(after_append[0].turns, 2);
        // preview still points at the first user message.
        assert_eq!(after_append[0].preview, "first message");

        // Deleting the file removes the entry from the next list (reconcile drops it).
        catalog.delete(&entry.name).unwrap();
        assert!(catalog.list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Best-effort mtime bump; some platforms/toolchains make `set_modified` awkward, so this
    /// is a small helper that is simply skipped if unavailable. Uses `File::set_times` via
    /// std (Rust 1.75+).
    fn filetime_set(path: &Path, when: std::time::SystemTime) -> std::io::Result<()> {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_modified(when)
    }

    #[test]
    fn create_lazy_does_not_touch_disk_until_first_append() {
        let dir = tmp_dir("lazy");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_lazy(None).unwrap();
        // After construction: the name is set, but there is no file in the directory (nothing written to disk).
        assert!(!entry.name.is_empty());
        assert!(catalog.list().unwrap().is_empty());
        assert!(!catalog.path_for(&entry.name).exists());
        // Before materialization, load is treated as an empty log.
        assert!(store.load().unwrap().is_empty());

        // Only the first append actually creates the file.
        store.append(&user("first entry")).unwrap();
        assert!(catalog.path_for(&entry.name).exists());
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, entry.name);
        assert_eq!(listed[0].preview, "first entry");
        assert_eq!(listed[0].turns, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_lazy_with_title_writes_sidecar_on_materialize() {
        let dir = tmp_dir("lazy_title");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_lazy(Some("migration task")).unwrap();
        assert_eq!(entry.label, "migration task");
        // Before materialization the title sidecar does not exist either.
        assert!(!catalog.title_path_for(&entry.name).exists());
        store.append(&user("get to work")).unwrap();
        let listed = catalog.list().unwrap();
        assert_eq!(listed[0].label, "migration task");
        assert_eq!(listed[0].preview, "get to work");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_lazy_append_truncate_round_trips_after_materialize() {
        let dir = tmp_dir("lazy_rt");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_lazy(None).unwrap();
        for c in ["a", "b", "c"] {
            store.append(&user(c)).unwrap();
        }
        // After materialization the behavior matches a normal session: truncate-tail + keep appending.
        store.truncate(2).unwrap();
        store.append(&user("d")).unwrap();
        let reopened = catalog.open(&entry.name).unwrap();
        assert_eq!(reopened.load().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_catalog_rejects_rename_and_delete() {
        let catalog = EmptySessionCatalog;
        assert!(catalog.rename("a", "b").is_err());
        assert!(catalog.delete("a").is_err());
    }

    #[test]
    fn first_user_message_skips_non_user_and_truncates() {
        let messages = vec![
            Message::Assistant {
                content: Some("hi".into()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            Message::User {
                content: "fix the login page styling".into(),
            },
            Message::User {
                content: "second message".into(),
            },
        ];
        assert_eq!(
            first_user_message(&messages).as_deref(),
            Some("fix the login page styling")
        );
        // No user message -> None.
        assert!(first_user_message(&[Message::Assistant {
            content: Some("x".into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
        }])
        .is_none());
        // Over-long input is truncated by character with an ellipsis.
        let long = "字".repeat(AUTO_TITLE_INPUT_CHARS + 50);
        let got = first_user_message(&[Message::User { content: long.into() }]).unwrap();
        assert!(got.ends_with('…'));
        assert_eq!(got.chars().count(), AUTO_TITLE_INPUT_CHARS + 1);
    }

    #[test]
    fn sanitize_title_strips_quotes_and_whitespace() {
        assert_eq!(sanitize_title("  “fix login page styles”  ").as_deref(), Some("fix login page styles"));
        assert_eq!(sanitize_title("`fix login`").as_deref(), Some("fix login"));
        // Take the first non-empty line.
        assert_eq!(sanitize_title("\n\nfirst title line\nsecond line").as_deref(), Some("first title line"));
        // Collapse internal whitespace.
        assert_eq!(sanitize_title("a   b\tc").as_deref(), Some("a b c"));
        // Empty after cleaning -> None.
        assert_eq!(sanitize_title("   \n  \n"), None);
        assert_eq!(sanitize_title("\"\""), None);
        // Over-long truncation.
        let long = "字".repeat(AUTO_TITLE_MAX_CHARS + 10);
        let got = sanitize_title(&long).unwrap();
        assert!(got.ends_with('…'));
    }
}
