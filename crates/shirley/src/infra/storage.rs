//! Global application state. Source workspaces contain no Shirley runtime files.
//! Configuration is shared; conversation/project-memory namespaces use canonical paths.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

/// The single, interface-agnostic state root: `~/.shirley`.
///
/// Both the desktop/TUI and the cloud/server share this one directory — running them on
/// the same machine under the same OS user means they intentionally see the same
/// `projects.json`, sessions, memory and `config.toml`. (This replaced the earlier
/// `<config_dir>/shirley` root, which was also per-user and therefore already shared;
/// the explicit home path just makes it predictable and identical across platforms.)
pub fn global_root() -> PathBuf {
    dirs::home_dir().map_or_else(
        || std::env::temp_dir().join("shirley"),
        |path| path.join(".shirley"),
    )
}

/// One-time migration of the legacy state root (`<config_dir>/shirley`) into the unified
/// root (`~/.shirley`). The desktop's real data used to live at the legacy path; on the
/// first run after the switch we move it over so nothing appears "lost".
///
/// Safe to call on every startup, including when the unified root already exists.
/// Existing project entries win; missing entries and legacy state are merged without
/// overwriting divergent files. Returns whether any data was migrated.
pub fn migrate_legacy_root() -> io::Result<bool> {
    let Some(legacy) = dirs::config_dir().map(|path| path.join("shirley")) else {
        return Ok(false);
    };
    let global = global_root();
    merge_legacy_root(&legacy, &global)
}

fn merge_legacy_root(legacy: &Path, global: &Path) -> io::Result<bool> {
    if !global.exists() {
        return migrate_root_from(legacy, global);
    }
    if !legacy.is_dir() || legacy == global {
        return Ok(false);
    }
    // An already-created destination must not hide older application data.
    let mut migrated = false;
    for entry in fs::read_dir(legacy)? {
        let entry = entry?;
        let source = entry.path();
        let target = global.join(entry.file_name());
        if entry.file_name() == "projects.json" && target.exists() {
            let mut current: serde_json::Value = serde_json::from_slice(&fs::read(&target)?)?;
            let old: serde_json::Value = serde_json::from_slice(&fs::read(&source)?)?;
            let projects = current
                .get_mut("projects")
                .and_then(|v| v.as_array_mut())
                .ok_or_else(|| io::Error::other("invalid current project registry"))?;
            let old_projects = old
                .get("projects")
                .and_then(|v| v.as_array())
                .ok_or_else(|| io::Error::other("invalid legacy project registry"))?;
            let before = projects.len();
            for project in old_projects {
                if !projects.iter().any(|p| p.get("id") == project.get("id")) {
                    projects.push(project.clone());
                }
            }
            if before != projects.len() {
                let temp = target.with_extension("migration.tmp");
                fs::write(&temp, serde_json::to_vec_pretty(&current)?)?;
                fs::rename(temp, target)?;
                migrated = true;
            }
            // Keep the old registry as evidence; current entries take precedence.
            continue;
        }
        preflight(&source, &target)?;
        merge_move(&source, &target)?;
        migrated = true;
    }
    Ok(migrated)
}

/// Move `legacy` onto `target` when the target does not exist yet. Falls back to a
/// recursive copy (cross-device) without deleting the source. Testable core of
/// [`migrate_legacy_root`].
fn migrate_root_from(legacy: &Path, target: &Path) -> io::Result<bool> {
    if legacy == target || !legacy.is_dir() || target.exists() {
        return Ok(false);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(()) = fs::rename(legacy, target) {
        Ok(true)
    } else {
        copy_tree(legacy, target)?;
        Ok(true)
    }
}

fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

pub fn config_path() -> PathBuf {
    global_root().join("config.toml")
}

#[derive(Debug)]
pub struct StoragePaths {
    pub global: PathBuf,
    pub workspace: PathBuf,
    pub state: PathBuf,
}

impl StoragePaths {
    pub fn new(global: PathBuf, workspace: &Path) -> io::Result<Self> {
        let workspace = fs::canonicalize(workspace)?;
        let id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            workspace.as_os_str().as_encoded_bytes(),
        );
        let state = global.join("workspaces").join(id.to_string());
        Ok(Self {
            global,
            workspace,
            state,
        })
    }

    #[cfg(test)]
    pub fn sessions(&self) -> PathBuf {
        self.state.join("sessions")
    }
    /// Bytes a chat page is allowed to open: `RenderPart`s point here by id, never by path.
    /// It is derived state (regenerable from a tool run), deliberately *not* session state.
    pub fn artifacts(&self) -> PathBuf {
        self.state.join("artifacts")
    }

    pub fn memory_roots(&self) -> Vec<PathBuf> {
        vec![self.global.join("memory"), self.state.join("memory")]
    }

    /// Reentrant across startup/project loading; no overwriting divergent data.
    /// Existing global settings win. Original project config is archived globally.
    pub fn migrate(&self) -> io::Result<bool> {
        fs::create_dir_all(&self.global)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.global.join(".migration.lock"))?;
        lock.lock()?;
        let legacy = self.workspace.join(".shirley");
        if legacy
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(io::Error::other(format!(
                "legacy state is a symlink: {}",
                legacy.display()
            )));
        }
        let has_legacy = legacy.exists();
        if has_legacy {
            // Detect conflicts/symlinks before promoting config or moving any files.
            for entry in fs::read_dir(&legacy)? {
                let entry = entry?;
                preflight(&entry.path(), &self.migration_target(&entry.file_name()))?;
            }
            promote_config(
                &legacy.join("config.toml"),
                &self.global.join("config.toml"),
            )?;
        }
        fs::create_dir_all(&self.state)?;
        let manifest = self.state.join("workspace.json");
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1, "workspace": self.workspace,
        }))?;
        if !manifest.exists() {
            fs::write(&manifest, bytes)?;
        }
        if has_legacy {
            for entry in fs::read_dir(&legacy)? {
                let entry = entry?;
                merge_move(&entry.path(), &self.migration_target(&entry.file_name()))?;
            }
            fs::remove_dir(&legacy)?;
        }
        Ok(has_legacy)
    }

    fn migration_target(&self, name: &std::ffi::OsStr) -> PathBuf {
        self.state.join(if name == "config.toml" {
            std::ffi::OsStr::new("legacy-config.toml")
        } else {
            name
        })
    }
}

/// Move the active workspace, Chat, and all registered projects (including archived ones).
pub fn migrate_all(current: &Path) -> io::Result<usize> {
    let global = global_root();
    migrate_chat_namespace(&global)?;
    let mut workspaces = vec![current.to_path_buf(), global.join("chat")];
    let registry = global.join("projects.json");
    if registry.exists() {
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&registry)?)?;
        if let Some(projects) = value.get("projects").and_then(|value| value.as_array()) {
            workspaces.extend(
                projects
                    .iter()
                    .filter_map(|entry| entry.get("path")?.as_str().map(PathBuf::from)),
            );
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut migrated = 0;
    for workspace in workspaces {
        if !workspace.is_dir() {
            continue;
        }
        let paths = StoragePaths::new(global.clone(), &workspace)?;
        if seen.insert(paths.workspace.clone()) && paths.migrate()? {
            migrated += 1;
        }
    }
    Ok(migrated)
}

/// Chat changed its absolute working path when the global root moved. Rebind its
/// existing namespace as well as its files, otherwise the new Chat appears empty.
fn migrate_chat_namespace(global: &Path) -> io::Result<()> {
    let chat = global.join("chat");
    fs::create_dir_all(&chat)?;
    let destination = StoragePaths::new(global.to_path_buf(), &chat)?;
    let Some(old_chat) = dirs::config_dir().map(|p| p.join("shirley/chat")) else {
        return Ok(());
    };
    let workspaces = global.join("workspaces");
    if !workspaces.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&workspaces)? {
        let source = entry?.path();
        if source == destination.state {
            continue;
        }
        let manifest = source.join("workspace.json");
        if !manifest.is_file() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&manifest)?)?;
        if value.get("workspace").and_then(|v| v.as_str()) != old_chat.to_str() {
            continue;
        }
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            if entry.file_name() != "workspace.json" {
                preflight(&entry.path(), &destination.state.join(entry.file_name()))?;
            }
        }
        destination.migrate()?;
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            if entry.file_name() != "workspace.json" {
                merge_move(&entry.path(), &destination.state.join(entry.file_name()))?;
            }
        }
        // Retain the old manifest for provenance; all active data now lives at destination.
    }
    Ok(())
}

fn promote_config(legacy: &Path, target: &Path) -> io::Result<()> {
    if !legacy.exists() {
        return Ok(());
    }
    let read = |path: &Path| -> io::Result<toml::Value> {
        if !path.exists() {
            return Ok(toml::Value::Table(toml::map::Map::default()));
        }
        let text = fs::read_to_string(path)?;
        // Reject malformed/unknown settings before changing anything.
        toml::from_str::<crate::settings::FileSettings>(&text).map_err(io::Error::other)?;
        toml::from_str(&text).map_err(io::Error::other)
    };
    let old = read(legacy)?;
    let mut global = read(target)?;
    let before = global.clone();
    fill_missing(&mut global, old);
    if global != before {
        let text = toml::to_string_pretty(&global).map_err(io::Error::other)?;
        let temp = target.with_extension(format!(
            "migration-{}",
            Uuid::new_v5(&Uuid::NAMESPACE_URL, legacy.as_os_str().as_encoded_bytes())
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, target)?;
    }
    Ok(())
}

fn fill_missing(global: &mut toml::Value, legacy: toml::Value) {
    if let (toml::Value::Table(global), toml::Value::Table(legacy)) = (global, legacy) {
        for (key, value) in legacy {
            match global.get_mut(&key) {
                Some(existing) => fill_missing(existing, value),
                None => {
                    global.insert(key, value);
                }
            }
        }
    }
}

fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    let (mut left, mut right) = ([0; 8192], [0; 8192]);
    loop {
        let n = a.read(&mut left)?;
        b.read_exact(&mut right[..n])?;
        if left[..n] != right[..n] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(b.read(&mut right[..1])? == 0);
        }
    }
}

fn preflight(source: &Path, target: &Path) -> io::Result<()> {
    let source_meta = fs::symlink_metadata(source)?;
    let target_meta = match fs::symlink_metadata(target) {
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if source_meta.file_type().is_symlink()
        || target_meta
            .as_ref()
            .is_some_and(|meta| meta.file_type().is_symlink())
    {
        return Err(io::Error::other(format!(
            "cannot migrate state symlink: {}",
            source.display()
        )));
    }
    if let Some(meta) = &target_meta
        && (source_meta.is_dir() != meta.is_dir()
            || (source_meta.is_file() && !same_contents(source, target)?))
    {
        return Err(io::Error::other(format!(
            "migration conflict; both files preserved: {} -> {}",
            source.display(),
            target.display()
        )));
    }
    if source_meta.is_dir() {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            preflight(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else if !source_meta.is_file() {
        return Err(io::Error::other(format!(
            "state is not a regular file: {}",
            source.display()
        )));
    }
    Ok(())
}

fn merge_move(source: &Path, target: &Path) -> io::Result<()> {
    if fs::symlink_metadata(source)?.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "cannot migrate a state symlink: {}",
            source.display()
        )));
    }
    if source.is_dir() {
        if !target.exists() {
            fs::create_dir_all(target.parent().unwrap())?;
            if fs::rename(source, target).is_ok() {
                return Ok(());
            }
        }
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            merge_move(&entry.path(), &target.join(entry.file_name()))?;
        }
        fs::remove_dir(source)?;
    } else {
        fs::create_dir_all(target.parent().unwrap())?;
        if target.exists() {
            if !same_contents(source, target)? {
                return Err(io::Error::other(format!(
                    "migration conflict; both files preserved: {} -> {}",
                    source.display(),
                    target.display()
                )));
            }
        } else if fs::rename(source, target).is_ok() {
            return Ok(());
        } else {
            fs::copy(source, target)?;
            if !same_contents(source, target)? {
                return Err(io::Error::other(
                    "state changed while copying; original preserved",
                ));
            }
        }
        fs::remove_file(source)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley-storage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn write(path: &Path, data: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    #[test]
    fn migrates_all_state_and_preserves_global_config_then_is_idempotent() {
        let dir = fixture();
        let workspace = dir.join("repo");
        let global = dir.join("global");
        write(
            &workspace.join(".shirley/config.toml"),
            "[provider]\nmodel='legacy'\nbase_url='http://example.test'\n[runtime]\nmax_steps=0\n",
        );
        write(
            &global.join("config.toml"),
            "[provider]\nmodel='global'\n[memory]\nembedding_model='m'\n",
        );
        for path in [
            "sessions/a.jsonl",
            "sessions/a.title",
            "sessions/a.agents/index.jsonl",
            "sessions/a.agents/sa_1.jsonl",
            "memory/facts/a.md",
            "memory/vectors.json",
            "config.toml.bak",
        ] {
            write(&workspace.join(".shirley").join(path), path);
        }
        let paths = StoragePaths::new(global.clone(), &workspace).unwrap();
        assert!(paths.migrate().unwrap());
        assert!(!workspace.join(".shirley").exists());
        assert_eq!(
            fs::read_to_string(paths.sessions().join("a.agents/sa_1.jsonl")).unwrap(),
            "sessions/a.agents/sa_1.jsonl"
        );
        assert!(paths.state.join("memory/vectors.json").exists());
        assert!(paths.state.join("legacy-config.toml").exists());
        assert!(paths.state.join("config.toml.bak").exists());
        let config: toml::Value =
            toml::from_str(&fs::read_to_string(global.join("config.toml")).unwrap()).unwrap();
        assert_eq!(config["provider"]["model"].as_str(), Some("global"));
        assert_eq!(
            config["provider"]["base_url"].as_str(),
            Some("http://example.test")
        );
        assert_eq!(config["runtime"]["max_steps"].as_integer(), Some(0));
        assert!(!paths.migrate().unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn projects_with_the_same_name_have_separate_global_state() {
        let dir = fixture();
        let a = dir.join("a/repo");
        let b = dir.join("b/repo");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let a = StoragePaths::new(dir.join("global"), &a).unwrap();
        let b = StoragePaths::new(dir.join("global"), &b).unwrap();
        assert_ne!(a.state, b.state);
        assert_eq!(a.memory_roots()[0], b.memory_roots()[0]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrate_legacy_root_moves_tree_once_and_is_idempotent() {
        let dir = fixture();
        let legacy = dir.join("legacy-shirley");
        let target = dir.join("home/.shirley");
        write(&legacy.join("projects.json"), "{\"version\":1}");
        write(&legacy.join("workspaces/abc/sessions/s.jsonl"), "hi");
        write(&legacy.join("config.toml"), "[provider]\nmodel='m'\n");

        assert!(migrate_root_from(&legacy, &target).unwrap());
        assert!(!legacy.exists(), "legacy tree should be moved away");
        assert_eq!(
            fs::read_to_string(target.join("projects.json")).unwrap(),
            "{\"version\":1}"
        );
        assert_eq!(
            fs::read_to_string(target.join("workspaces/abc/sessions/s.jsonl")).unwrap(),
            "hi"
        );
        assert!(target.join("config.toml").exists());

        // Second run is a no-op (target already exists).
        assert!(!migrate_root_from(&legacy, &target).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn conflicting_sessions_are_never_overwritten() {
        let dir = fixture();
        let workspace = dir.join("repo");
        write(&workspace.join(".shirley/sessions/a.jsonl"), "source");
        let paths = StoragePaths::new(dir.join("global"), &workspace).unwrap();
        write(&paths.sessions().join("a.jsonl"), "target");
        assert!(
            paths
                .migrate()
                .unwrap_err()
                .to_string()
                .contains("conflict")
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".shirley/sessions/a.jsonl")).unwrap(),
            "source"
        );
        assert_eq!(
            fs::read_to_string(paths.sessions().join("a.jsonl")).unwrap(),
            "target"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rebinds_legacy_chat_namespace_and_keeps_session_sidecars() {
        let dir = fixture();
        let old_chat = dirs::config_dir().unwrap().join("shirley/chat");
        let old_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            old_chat.as_os_str().as_encoded_bytes(),
        );
        let old = dir.join("workspaces").join(old_id.to_string());
        write(
            &old.join("workspace.json"),
            &serde_json::json!({"version":1,"workspace":old_chat}).to_string(),
        );
        write(&old.join("sessions/old.jsonl"), "history");
        write(&old.join("sessions/old.title"), "title");
        write(&old.join("sessions/old.agents/child.jsonl"), "child");
        migrate_chat_namespace(&dir).unwrap();
        let paths = StoragePaths::new(dir.clone(), &dir.join("chat")).unwrap();
        assert_eq!(
            fs::read_to_string(paths.sessions().join("old.jsonl")).unwrap(),
            "history"
        );
        assert_eq!(
            fs::read_to_string(paths.sessions().join("old.title")).unwrap(),
            "title"
        );
        assert_eq!(
            fs::read_to_string(paths.sessions().join("old.agents/child.jsonl")).unwrap(),
            "child"
        );
        migrate_chat_namespace(&dir).unwrap();
        assert!(!old.join("sessions").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn merges_legacy_root_even_when_destination_exists() {
        let dir = fixture();
        let legacy = dir.join("old");
        let global = dir.join("new");
        write(
            &legacy.join("projects.json"),
            r#"{"version":1,"projects":[{"id":"a","name":"old"},{"id":"b"}]}"#,
        );
        write(
            &global.join("projects.json"),
            r#"{"version":1,"projects":[{"id":"a","name":"current"}]}"#,
        );
        write(&legacy.join("workspaces/id/sessions/old.jsonl"), "history");
        assert!(merge_legacy_root(&legacy, &global).unwrap());
        let registry: serde_json::Value =
            serde_json::from_slice(&fs::read(global.join("projects.json")).unwrap()).unwrap();
        assert_eq!(registry["projects"].as_array().unwrap().len(), 2);
        assert_eq!(registry["projects"][0]["name"], "current");
        assert_eq!(
            fs::read_to_string(global.join("workspaces/id/sessions/old.jsonl")).unwrap(),
            "history"
        );
        assert!(!merge_legacy_root(&legacy, &global).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
