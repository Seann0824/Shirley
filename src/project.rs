//! Application-side project registry (multi-workspace / multi-project).
//!
//! The desktop interface lets users register **multiple workspaces** as "projects", each with its own sessions.
//! This module handles exactly one thing: **which directories are projects** -- one global registry across workspaces,
//! persisted at `<config_dir>/shirley/projects.json` (same root as global memory, see `docs/memory.md`).
//!
//! **This is an application-layer concern, not in the SDK**: the SDK only knows a single `working_dir`; "which projects
//! I have open at once" is desktop-interface orchestration, not a base capability.
//!
//! Session logs still land in **each project's own** `<config_dir>/shirley/workspaces/<workspace-id>/sessions/` (stored globally, isolated by canonical workspace path) -- the registry only records "where the project is", runtime state is resolved by `crate::storage`.
//!
//! # Archive semantics
//!
//! "Remove project" = **archive**: only sets `archived = true`, **deletes nothing on disk**
//! (sessions, memory, and the repo are all preserved). Archived projects do not appear in the sidebar by default and can be
//! **restored** from the archive list. This makes mis-clicks reversible -- users complained that "remove" was too scary.
//!
//! # Identity
//!
//! Project `id` = the string form of the directory's **canonical path** (after canonicalize). When the same directory is
//! added repeatedly it is deduped by id (if it was previously archived, it is **restored and renamed**, rather than adding a new entry).

// Desktop-only module: no consumer in the default (TUI-only) build; `dead_code` is expected noise.
#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Registry error. `Io` covers read/write failures, `Canonicalize` covers invalid paths (nonexistent / no permission).
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("failed to read/write the project registry: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse the project registry: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("invalid path: {0}")]
    Canonicalize(String),
    #[error("project not found: {0}")]
    NotFound(String),
}

/// A registered project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectEntry {
    /// Canonical path string -- the project's stable identity (dedupe key).
    pub id: String,
    /// Display name (renameable).
    pub name: String,
    /// Workspace root directory.
    pub path: PathBuf,
    /// Whether archived (archived projects are hidden by default and can be restored).
    #[serde(default)]
    pub archived: bool,
    /// First-registered time (milliseconds).
    #[serde(default)]
    pub created_ms: u64,
    /// Most recently opened time (milliseconds) -- the list is sorted by it in descending order.
    #[serde(default)]
    pub last_opened_ms: u64,
}

/// On-disk registry structure (versioned, to ease future migration).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    projects: Vec<ProjectEntry>,
}

/// Current registry file version.
const REGISTRY_VERSION: u32 = 1;

/// Project registry: in-memory state + on-disk path.
pub struct ProjectRegistry {
    path: PathBuf,
    projects: Vec<ProjectEntry>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Fall back to the directory name as the display name (use the full path when canonicalization fails / there is no file name).
fn default_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

impl ProjectRegistry {
    /// Load using the real `config_dir` (degrades to an in-memory, non-persisted empty table when `config_dir` is unavailable).
    pub fn load_default() -> Result<Self, ProjectError> {
        let path = crate::storage::global_root().join("projects.json");
        Self::load(path)
    }

    /// Load from the given path (missing file = empty table). Tests inject a path.
    pub fn load(path: PathBuf) -> Result<Self, ProjectError> {
        let projects = match std::fs::read(&path) {
            Ok(bytes) => {
                let file: RegistryFile = serde_json::from_slice(&bytes)?;
                file.projects
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(ProjectError::Io(error)),
        };
        Ok(Self { path, projects })
    }

    /// Persist (create the directory, write a temp file, then atomically replace, to avoid a half-written corruption).
    fn save(&self) -> Result<(), ProjectError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = RegistryFile {
            version: REGISTRY_VERSION,
            projects: self.projects.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// List projects (most recently opened first). With `include_archived = false`, archived projects are hidden.
    pub fn list(&self, include_archived: bool) -> Vec<&ProjectEntry> {
        let mut entries: Vec<&ProjectEntry> = self
            .projects
            .iter()
            .filter(|entry| include_archived || !entry.archived)
            .collect();
        entries.sort_by(|a, b| b.last_opened_ms.cmp(&a.last_opened_ms));
        entries
    }

    /// Get a project by id.
    pub fn get(&self, id: &str) -> Option<&ProjectEntry> {
        self.projects.iter().find(|entry| entry.id == id)
    }

    /// Register a project (deduped by canonical path).
    ///
    /// - Directory not registered before -> add a new entry.
    /// - Registered but **archived** -> restore it (`archived = false`) and overwrite the display name with `name`.
    /// - Registered and not archived -> only refresh `last_opened_ms` (no duplicate registration).
    ///
    /// Returns the registered entry (a clone).
    pub fn add(&mut self, path: PathBuf, name: Option<String>) -> Result<ProjectEntry, ProjectError> {
        let canonical = std::fs::canonicalize(&path)
            .map_err(|error| ProjectError::Canonicalize(format!("{}: {error}", path.display())))?;
        let id = canonical.to_string_lossy().into_owned();
        let display = name
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| default_name(&canonical));
        let ts = now_ms();

        if let Some(existing) = self.projects.iter_mut().find(|entry| entry.id == id) {
            existing.archived = false;
            existing.name = display;
            existing.path = canonical;
            existing.last_opened_ms = ts;
            let out = existing.clone();
            self.save()?;
            return Ok(out);
        }

        let entry = ProjectEntry {
            id,
            name: display,
            path: canonical,
            archived: false,
            created_ms: ts,
            last_opened_ms: ts,
        };
        self.projects.push(entry.clone());
        self.save()?;
        Ok(entry)
    }

    /// Rename (changes only the display name, not the path / session data).
    pub fn rename(&mut self, id: &str, name: &str) -> Result<(), ProjectError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ProjectError::NotFound(id.to_owned()));
        }
        let entry = self
            .projects
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| ProjectError::NotFound(id.to_owned()))?;
        entry.name = name.to_owned();
        self.save()
    }

    /// Archive (does not delete on-disk data, only sets a flag).
    pub fn archive(&mut self, id: &str) -> Result<(), ProjectError> {
        let entry = self
            .projects
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| ProjectError::NotFound(id.to_owned()))?;
        entry.archived = true;
        self.save()
    }

    /// Restore from the archive and refresh the last-opened time.
    pub fn restore(&mut self, id: &str) -> Result<(), ProjectError> {
        let ts = now_ms();
        let entry = self
            .projects
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| ProjectError::NotFound(id.to_owned()))?;
        entry.archived = false;
        entry.last_opened_ms = ts;
        self.save()
    }

    /// Refresh the last-opened time (called when opening a project; best-effort persist).
    pub fn touch(&mut self, id: &str) {
        if let Some(entry) = self.projects.iter_mut().find(|entry| entry.id == id) {
            entry.last_opened_ms = now_ms();
            let _ = self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_registry(tag: &str) -> (PathBuf, ProjectRegistry) {
        let dir = std::env::temp_dir().join(format!(
            "shirley_projects_{tag}_{}_{}",
            std::process::id(),
            now_ms()
        ));
        let path = dir.join("projects.json");
        let reg = ProjectRegistry::load(path.clone()).expect("load");
        (dir, reg)
    }

    fn make_dir(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn add_and_list_round_trips() {
        let (root, mut reg) = temp_registry("add");
        let a = make_dir(&root, "alpha");
        let b = make_dir(&root, "beta");
        let ea = reg.add(a.clone(), None).unwrap();
        let eb = reg.add(b.clone(), Some("My Project".into())).unwrap();
        assert_eq!(ea.name, "alpha");
        assert_eq!(eb.name, "My Project");

        let listed = reg.list(false);
        assert_eq!(listed.len(), 2);
        // Both ids are present (order is a UI concern, see the invariant assertion below).
        assert!(listed.iter().any(|entry| entry.id == ea.id));
        assert!(listed.iter().any(|entry| entry.id == eb.id));
        // Invariant: the list is sorted by last_opened_ms descending (ties within the same millisecond are acceptable).
        for pair in listed.windows(2) {
            assert!(pair[0].last_opened_ms >= pair[1].last_opened_ms);
        }

        // Reloading reads it back.
        let reloaded = ProjectRegistry::load(reg.path.clone()).unwrap();
        assert_eq!(reloaded.list(false).len(), 2);
    }

    #[test]
    fn add_is_idempotent_and_dedupes_by_canonical_path() {
        let (root, mut reg) = temp_registry("dedupe");
        let a = make_dir(&root, "alpha");
        let first = reg.add(a.clone(), None).unwrap();
        // Adding the same directory (including a ./ variant) again: no new entry, only a refresh.
        let again = reg.add(a.join("."), Some("renamed".into())).unwrap();
        assert_eq!(first.id, again.id);
        assert_eq!(reg.list(false).len(), 1);
        assert_eq!(again.name, "renamed");
    }

    #[test]
    fn archive_hides_and_restore_brings_back() {
        let (root, mut reg) = temp_registry("archive");
        let a = make_dir(&root, "alpha");
        let entry = reg.add(a, None).unwrap();

        reg.archive(&entry.id).unwrap();
        assert!(reg.list(false).is_empty());
        assert_eq!(reg.list(true).len(), 1);
        assert!(reg.get(&entry.id).unwrap().archived);

        reg.restore(&entry.id).unwrap();
        assert_eq!(reg.list(false).len(), 1);
        assert!(!reg.get(&entry.id).unwrap().archived);
    }

    #[test]
    fn add_after_archive_restores_instead_of_duplicating() {
        let (root, mut reg) = temp_registry("readd");
        let a = make_dir(&root, "alpha");
        let entry = reg.add(a.clone(), None).unwrap();
        reg.archive(&entry.id).unwrap();
        let again = reg.add(a, None).unwrap();
        assert_eq!(again.id, entry.id);
        assert!(!again.archived);
        assert_eq!(reg.list(true).len(), 1);
    }

    #[test]
    fn rename_changes_label_only() {
        let (root, mut reg) = temp_registry("rename");
        let a = make_dir(&root, "alpha");
        let entry = reg.add(a, None).unwrap();
        reg.rename(&entry.id, "new name").unwrap();
        let got = reg.get(&entry.id).unwrap();
        assert_eq!(got.name, "new name");
        assert_eq!(got.path, entry.path);
    }

    #[test]
    fn add_rejects_nonexistent_path() {
        let (root, mut reg) = temp_registry("missing");
        let missing = root.join("does-not-exist");
        assert!(matches!(
            reg.add(missing, None),
            Err(ProjectError::Canonicalize(_))
        ));
    }

    #[test]
    fn rename_unknown_id_errors() {
        let (_root, mut reg) = temp_registry("unknown");
        assert!(matches!(
            reg.rename("nope", "x"),
            Err(ProjectError::NotFound(_))
        ));
    }
}
