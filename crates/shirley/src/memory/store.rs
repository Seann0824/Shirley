//! Memory persistence layer: reading and writing a Markdown directory (`docs/memory.md` 3.1).
//!
//! Layout (two levels: global personal memory + workspace project memory):
//!
//! ```text
//! ~/.shirley/memory/   # global (cross-project)
//! |-- core.md                    # resident layer: profile + explicit preferences + active projects
//! |-- index.md                   # program-maintained index page (see index.rs)
//! |-- episodic/                  # episodic memory (events)
//! |-- preferences/               # preferences / habits
//! |-- procedures/                # procedural memory (behavior flows)
//! |-- facts/                     # facts
//! ~/.shirley/workspaces/<workspace-id>/memory/        # workspace level (stored globally)
//! ```
//!
//! Writes select a root by domain; reads merge all roots, and for the same `id` a later one
//! overrides an earlier one (workspace overrides global). Uses synchronous `std::fs`: the memory directory is small, and
//! `ContextProvider::context()` is a synchronous interface, so reads must be synchronous.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::format::{Domain, Entry, EntryType, MemoryError};

/// Resident overview file name.
pub const CORE_FILE: &str = "core.md";
/// Index page file name.
pub const INDEX_FILE: &str = "index.md";

/// Entry subdirectories (`type` -> directory, see [`dir_for`]).
pub const ENTRY_DIRS: [&str; 4] = ["episodic", "preferences", "procedures", "facts"];

/// Memory store: holds several root directories (the first is the write primary root).
pub struct MemoryStore {
    roots: Vec<PathBuf>,
}

impl MemoryStore {
    /// Single-root store (tests / workspace level).
    ///
    /// The V1 production path uniformly uses [`MemoryStore::with_roots`] (global + workspace dual roots);
    /// this constructor is used only by tests and a future "single-root workspace" scenario.
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            roots: vec![root.into()],
        }
    }

    /// Multi-root store (global + workspace). `roots[0]` is the write primary root.
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        assert!(!roots.is_empty(), "memory store needs at least one root");
        Self { roots }
    }

    /// The write primary root.
    pub fn primary_root(&self) -> &Path {
        &self.roots[0]
    }

    /// All root directories (read merge order: global -> workspace). Vector sidecars are looked up per root / written to the primary root.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub(crate) fn lock_roots(&self) -> Result<Vec<std::fs::File>, MemoryError> {
        self.roots
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|root| lock_root(root))
            .collect()
    }

    /// Reject stale model proposals if a sync or another curator changed their inputs.
    pub(crate) fn matches_snapshot(
        &self,
        expected: &std::collections::HashMap<String, Entry>,
    ) -> Result<bool, MemoryError> {
        let normalize = |mut entry: Entry| {
            entry.usage_count = None;
            entry.utility = None;
            entry
        };
        let current: std::collections::HashMap<_, _> = self
            .list_entries()?
            .into_iter()
            .map(|(_, entry)| (entry.id.clone(), normalize(entry)))
            .collect();
        let expected: std::collections::HashMap<_, _> = expected
            .iter()
            .map(|(id, entry)| (id.clone(), normalize(entry.clone())))
            .collect();
        Ok(current == expected)
    }

    /// Create all entry subdirectories (idempotent). `core.md` / `index.md` are created by the writer on demand.
    ///
    /// The write path now uses [`Self::ensure_layout_at`] (selecting a root by domain); this method is kept as a public contract / for tests.
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
    pub fn ensure_layout(&self) -> Result<(), MemoryError> {
        Self::ensure_layout_at(self.primary_root())
    }

    /// Create entry subdirectories under the given root (idempotent).
    pub fn ensure_layout_at(root: &Path) -> Result<(), MemoryError> {
        std::fs::create_dir_all(root)?;
        for dir in ENTRY_DIRS {
            std::fs::create_dir_all(root.join(dir))?;
        }
        Ok(())
    }

    /// Select the storage root by **owning domain**: `Personal` -> global root (`roots[0]`), `Project` -> workspace root
    /// (`roots[1]`, i.e. `~/.shirley/workspaces/<workspace-id>/memory`). With a single-root store (tests), `Project` falls back to the primary root.
    pub fn root_for_domain(&self, domain: Domain) -> &Path {
        match domain {
            Domain::Personal => self.primary_root(),
            Domain::Project => self
                .roots
                .get(1)
                .map_or_else(|| self.primary_root(), PathBuf::as_path),
        }
    }

    /// Read the resident overview `core.md` (multi-root merge: workspace content is appended after global). Empty / missing returns `None`.
    pub fn read_core(&self) -> Option<String> {
        let mut parts = Vec::new();
        for root in &self.roots {
            if let Ok(text) = std::fs::read_to_string(root.join(CORE_FILE)) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
            }
        }
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    /// Write the resident overview `core.md` (lands in the primary root).
    ///
    /// V1's `core.md` is maintained explicitly by the user / app, and the curator does not write it; this method is for tests and
    /// a future "auto-maintain core" use.
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
    pub fn write_core(&self, text: &str) -> Result<(), MemoryError> {
        self.write_core_at(self.primary_root(), text)
    }

    /// Write the resident overview to a **specified root** (for per-root rebuilds).
    #[expect(
        clippy::unused_self,
        reason = "kept as an associated method for call-site symmetry with other store methods"
    )]
    pub fn write_core_at(&self, root: &Path, text: &str) -> Result<(), MemoryError> {
        Self::ensure_layout_at(root)?;
        atomic_write(&root.join(CORE_FILE), text.as_bytes())?;
        Ok(())
    }

    /// The absolute path an entry should land at: `<root_for_domain>/<dir>/<id>.md`.
    pub fn entry_path(&self, entry: &Entry) -> PathBuf {
        self.root_for_domain(entry.domain)
            .join(dir_for(entry.entry_type))
            .join(format!("{}.md", entry.id))
    }

    /// Write one entry (**selecting the root by `entry.domain`**; `id` determines the file name). Returns the written path.
    #[cfg(test)]
    pub fn write_entry(&self, entry: &Entry) -> Result<PathBuf, MemoryError> {
        let root = self.root_for_domain(entry.domain);
        let _lock = lock_root(root)?;
        self.write_entry_unlocked(entry)
    }

    pub(crate) fn write_entry_unlocked(&self, entry: &Entry) -> Result<PathBuf, MemoryError> {
        validate_id(&entry.id)?;
        let root = self.root_for_domain(entry.domain);
        Self::ensure_layout_at(root)?;
        let path = self.entry_path(entry);
        let previous = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| Entry::parse(&text).ok());
        if previous
            .as_ref()
            .is_none_or(|old| old.subject != entry.subject || old.body != entry.body)
        {
            super::vector::invalidate(root, &entry.id)?;
        }
        let mut rendered = entry.render();
        // A usage-statistics/curation rewrite must not silently enroll legacy entries in cloud sync.
        if entry.domain == Domain::Personal
            && std::fs::read_to_string(&path)
                .is_ok_and(|old| !super::format::has_explicit_domain(&old))
        {
            rendered = rendered.replacen("domain: personal\n", "", 1);
        }
        atomic_write(&path, rendered.as_bytes())?;
        Ok(path)
    }

    /// Read one entry.
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
    #[expect(
        clippy::unused_self,
        reason = "kept as an associated method for call-site symmetry with other store methods"
    )]
    pub fn read_entry(&self, path: &Path) -> Result<Entry, MemoryError> {
        let text = std::fs::read_to_string(path)?;
        Entry::parse(&text)
    }

    /// Traverse all roots and all entry directories, parse out all entries, and return them sorted ascending by `id`.
    ///
    /// For the same `id`, a later one overrides an earlier one (workspace overrides global). Entries that fail to parse are **skipped**
    /// (do not let one bad file bring down the whole store), returning `(path, entry)`.
    pub fn list_entries(&self) -> Result<Vec<(PathBuf, Entry)>, MemoryError> {
        let mut by_id: BTreeMap<String, (PathBuf, Entry)> = BTreeMap::new();
        for root in &self.roots {
            for dir in ENTRY_DIRS {
                let dir_path = root.join(dir);
                let read_dir = match std::fs::read_dir(&dir_path) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                for item in read_dir {
                    let path = item?.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    let Ok(entry) = Entry::parse(&text) else {
                        continue;
                    };
                    by_id.insert(entry.id.clone(), (path, entry));
                }
            }
        }
        Ok(by_id.into_values().collect())
    }

    /// Traverse entries of a **single root** (for per-root rebuilds of `core.md` / `index.md`). Sorted ascending by `id`.
    ///
    /// Unlike [`Self::list_entries`] (cross-root merge, same-name override): here only the given root is considered, which naturally separates
    /// global (personal) from workspace (project) -- per-root rebuilds each project their own, without bleeding into each other.
    #[expect(
        clippy::unused_self,
        reason = "kept as an associated method for call-site symmetry with other store methods"
    )]
    pub fn list_entries_in(&self, root: &Path) -> Result<Vec<(PathBuf, Entry)>, MemoryError> {
        let mut by_id: BTreeMap<String, (PathBuf, Entry)> = BTreeMap::new();
        for dir in ENTRY_DIRS {
            let dir_path = root.join(dir);
            let read_dir = match std::fs::read_dir(&dir_path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for item in read_dir {
                let path = item?.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(entry) = Entry::parse(&text) else {
                    continue;
                };
                by_id.insert(entry.id.clone(), (path, entry));
            }
        }
        Ok(by_id.into_values().collect())
    }

    /// Express an absolute path as a display path relative to some root (for the index page).
    pub fn relative_path(&self, path: &Path) -> String {
        for root in &self.roots {
            if let Ok(rel) = path.strip_prefix(root) {
                return rel.to_string_lossy().replace('\\', "/");
            }
        }
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// Delete one entry (find by `id` across all roots, delete on find). Returns whether anything was deleted.
    ///
    /// `delete` is one of the curator's actions (when a candidate is falsified by environment probing).
    ///
    /// The V1 curator only does upsert + supersede and does not yet wire deletion; kept as a store contract.
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
    pub fn delete_entry(&self, id: &str) -> Result<bool, MemoryError> {
        validate_id(id)?;
        let mut removed = false;
        for root in &self.roots {
            let _lock = lock_root(root)?;
            super::vector::invalidate(root, id)?;
            for dir in ENTRY_DIRS {
                let path = root.join(dir).join(format!("{id}.md"));
                match std::fs::remove_file(&path) {
                    Ok(()) => removed = true,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(removed)
    }
}

pub(crate) fn validate_id(id: &str) -> Result<(), MemoryError> {
    if id.is_empty()
        || id.len() > 200
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(MemoryError::Parse(
            "memory id must be a slug of at most 200 bytes".into(),
        ));
    }
    Ok(())
}

/// OS lock, shared by curation, desktop, TUI and the sync endpoint. Never held over HTTP.
pub(crate) fn lock_root(root: &Path) -> Result<std::fs::File, MemoryError> {
    std::fs::create_dir_all(root)?;
    if std::fs::symlink_metadata(root.join(".write.lock"))
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(MemoryError::Parse("memory lock cannot be a symlink".into()));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".write.lock"))?;
    file.lock()?;
    Ok(file)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), MemoryError> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| MemoryError::Parse("missing parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let nonce = format!("{}-{:?}", std::process::id(), std::thread::current().id());
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        nonce
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(Into::into)
}

/// `type` -> entry subdirectory.
fn dir_for(entry_type: EntryType) -> &'static str {
    match entry_type {
        EntryType::Event => "episodic",
        EntryType::Preference => "preferences",
        EntryType::Procedure => "procedures",
        EntryType::Fact => "facts",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::format::{Confidence, EntryStatus};

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_mem_store_{tag}_{}_{}",
            std::process::id(),
            // Make multiple calls within the same process get different directories.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn entry(id: &str, entry_type: EntryType, body: &str) -> Entry {
        Entry {
            id: id.into(),
            entry_type,
            domain: Domain::Personal,
            subject: "s".into(),
            created_at: "2026-05-10".into(),
            valid_from: None,
            supersedes: None,
            status: EntryStatus::Active,
            confidence: Confidence::High,
            scope: None,
            utility: None,
            usage_count: None,
            source: vec!["2026-05-10-abc.jsonl#turn:1".into()],
            body: body.into(),
        }
    }

    #[test]
    fn ensure_layout_creates_dirs() {
        let root = tmp("layout");
        let store = MemoryStore::new(&root);
        store.ensure_layout().unwrap();
        for dir in ENTRY_DIRS {
            assert!(root.join(dir).is_dir(), "{dir} should exist");
        }
    }

    #[test]
    fn entry_round_trips_via_disk() {
        let root = tmp("roundtrip");
        let store = MemoryStore::new(&root);
        let original = entry("pref-a", EntryType::Preference, "Preference A.");
        let path = store.write_entry(&original).unwrap();
        assert!(path.ends_with("preferences/pref-a.md"));

        let listed = store.list_entries().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1, original);
    }

    #[test]
    fn core_round_trips() {
        let root = tmp("core");
        let store = MemoryStore::new(&root);
        assert!(store.read_core().is_none());
        store.write_core("The user is Sean.").unwrap();
        assert_eq!(store.read_core().as_deref(), Some("The user is Sean."));
    }

    #[test]
    fn workspace_root_overrides_global_for_same_id() {
        let global = tmp("global");
        let workspace = tmp("workspace");
        let g = MemoryStore::new(&global);
        let w = MemoryStore::new(&workspace);
        g.write_entry(&entry("fact-db", EntryType::Fact, "Global: uses MySQL."))
            .unwrap();
        w.write_entry(&entry(
            "fact-db",
            EntryType::Fact,
            "Workspace: uses Postgres.",
        ))
        .unwrap();

        // roots[0]=global (write), roots[1]=workspace (overrides on read).
        let merged = MemoryStore::with_roots(vec![global.clone(), workspace.clone()]);
        let listed = merged.list_entries().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.body, "Workspace: uses Postgres.");
    }

    #[test]
    fn core_merges_global_then_workspace() {
        let global = tmp("core_g");
        let workspace = tmp("core_w");
        MemoryStore::new(&global)
            .write_core("Global: the user is Sean.")
            .unwrap();
        MemoryStore::new(&workspace)
            .write_core("Workspace: migrating.")
            .unwrap();
        let merged = MemoryStore::with_roots(vec![global, workspace]);
        assert_eq!(
            merged.read_core().as_deref(),
            Some("Global: the user is Sean.\n\nWorkspace: migrating.")
        );
    }

    #[test]
    fn malformed_entry_is_skipped() {
        let root = tmp("malformed");
        let store = MemoryStore::new(&root);
        store.ensure_layout().unwrap();
        std::fs::write(root.join("facts/broken.md"), "not a valid entry").unwrap();
        store
            .write_entry(&entry("fact-ok", EntryType::Fact, "Fine."))
            .unwrap();
        let listed = store.list_entries().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.id, "fact-ok");
    }

    #[test]
    fn delete_entry_removes_file() {
        let root = tmp("delete");
        let store = MemoryStore::new(&root);
        store
            .write_entry(&entry("fact-gone", EntryType::Fact, "Delete me."))
            .unwrap();
        assert!(store.delete_entry("fact-gone").unwrap());
        assert!(store.list_entries().unwrap().is_empty());
        assert!(!store.delete_entry("fact-gone").unwrap());
    }
}
