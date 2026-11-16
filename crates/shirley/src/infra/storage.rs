//! Global application state. Source workspaces contain no Shirley runtime files.
//! Configuration is shared; conversation/project-memory namespaces use canonical paths.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};


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

/// Where application state lives. `global` is `~/.shirley`; `state` is where the platform
/// factory keeps its sessions and artifacts (currently the same directory -- bots get their
/// own subtree under `bots/<name>/`, see `AgentFactory::for_bot`).
#[derive(Debug)]
pub struct StoragePaths {
    pub global: PathBuf,
    pub state: PathBuf,
}

impl StoragePaths {
    pub fn for_global(global: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&global)?;
        Ok(Self {
            state: global.clone(),
            global,
        })
    }

    /// Bytes a chat page is allowed to open: `RenderPart`s point here by id, never by path.
    /// It is derived state (regenerable from a tool run), deliberately *not* session state.
    pub fn artifacts(&self) -> PathBuf {
        self.state.join("artifacts")
    }

    pub fn memory_roots(&self) -> Vec<PathBuf> {
        let global_mem = self.global.join("memory");
        let state_mem = self.state.join("memory");
        if global_mem == state_mem {
            vec![global_mem]
        } else {
            vec![global_mem, state_mem]
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
    fn global_paths_share_one_memory_root() {
        let dir = fixture();
        let paths = StoragePaths::for_global(dir.join("global")).unwrap();
        assert_eq!(paths.state, paths.global);
        assert_eq!(paths.memory_roots(), vec![dir.join("global/memory")]);
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
    fn merges_legacy_root_even_when_destination_exists() {
        let dir = fixture();
        let legacy = dir.join("old");
        let global = dir.join("new");
        write(&global.join("config.toml"), "[provider]\nmodel='m'\n");
        write(&legacy.join("workspaces/id/sessions/old.jsonl"), "history");
        assert!(merge_legacy_root(&legacy, &global).unwrap());
        assert_eq!(
            fs::read_to_string(global.join("workspaces/id/sessions/old.jsonl")).unwrap(),
            "history"
        );
        assert!(!merge_legacy_root(&legacy, &global).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
