//! Memory persistence layer: reading and writing a Markdown directory (`docs/memory.md` 3.1).
//!
//! Layout (two levels: global personal memory + workspace project memory):
//!
//! ```text
//! <config_dir>/shirley/memory/   # global (cross-project)
//! |-- core.md                    # resident layer: profile + explicit preferences + active projects
//! |-- index.md                   # program-maintained index page (see index.rs)
//! |-- episodic/                  # episodic memory (events)
//! |-- preferences/               # preferences / habits
//! |-- procedures/                # procedural memory (behavior flows)
//! |-- facts/                     # facts
//! <config_dir>/shirley/workspaces/<workspace-id>/memory/        # workspace level (stored globally)
//! ```
//!
//! **Writes only land in the primary root** (`roots[0]`, usually global); **reads merge all roots**, and for the same `id` a later one
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
    #[allow(dead_code)]
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

    /// Create all entry subdirectories (idempotent). `core.md` / `index.md` are created by the writer on demand.
    ///
    /// The write path now uses [`Self::ensure_layout_at`] (selecting a root by domain); this method is kept as a public contract / for tests.
    #[allow(dead_code)]
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
    /// (`roots[1]`, i.e. `<config_dir>/shirley/workspaces/<workspace-id>/memory`). With a single-root store (tests), `Project` falls back to the primary root.
    pub fn root_for_domain(&self, domain: Domain) -> &Path {
        match domain {
            Domain::Personal => self.primary_root(),
            Domain::Project => self
                .roots
                .get(1)
                .map(PathBuf::as_path)
                .unwrap_or_else(|| self.primary_root()),
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
    #[allow(dead_code)]
    pub fn write_core(&self, text: &str) -> Result<(), MemoryError> {
        self.write_core_at(self.primary_root(), text)
    }

    /// Write the resident overview to a **specified root** (for per-root rebuilds).
    pub fn write_core_at(&self, root: &Path, text: &str) -> Result<(), MemoryError> {
        Self::ensure_layout_at(root)?;
        std::fs::write(root.join(CORE_FILE), text)?;
        Ok(())
    }

    /// The absolute path an entry should land at: `<root_for_domain>/<dir>/<id>.md`.
    pub fn entry_path(&self, entry: &Entry) -> PathBuf {
        self.root_for_domain(entry.domain)
            .join(dir_for(entry.entry_type))
            .join(format!("{}.md", entry.id))
    }

    /// Write one entry (**selecting the root by `entry.domain`**; `id` determines the file name). Returns the written path.
    pub fn write_entry(&self, entry: &Entry) -> Result<PathBuf, MemoryError> {
        let root = self.root_for_domain(entry.domain);
        Self::ensure_layout_at(root)?;
        let path = self.entry_path(entry);
        std::fs::write(&path, entry.render())?;
        Ok(path)
    }

    /// Read one entry.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn delete_entry(&self, id: &str) -> Result<bool, MemoryError> {
        let mut removed = false;
        for root in &self.roots {
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
    use crate::memory::format::{Confidence, Domain, EntryStatus};

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
        w.write_entry(&entry("fact-db", EntryType::Fact, "Workspace: uses Postgres."))
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
        MemoryStore::new(&global).write_core("Global: the user is Sean.").unwrap();
        MemoryStore::new(&workspace).write_core("Workspace: migrating.").unwrap();
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
