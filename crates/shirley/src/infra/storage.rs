//! Where application state lives on disk.

use std::fs;
use std::io;
use std::path::PathBuf;

/// The single state root: `~/.shirley` (config, shared memory, task queue, `bots/`).
pub fn global_root() -> PathBuf {
    dirs::home_dir().map_or_else(
        || std::env::temp_dir().join("shirley"),
        |path| path.join(".shirley"),
    )
}

/// Where bot definitions and per-bot state live: `~/.shirley/bots/<name>/`.
pub fn bots_root() -> PathBuf {
    global_root().join("bots")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_paths_share_one_memory_root() {
        let dir = std::env::temp_dir().join(format!("shirley-storage-{}", std::process::id()));
        let paths = StoragePaths::for_global(dir.join("global")).unwrap();
        assert_eq!(paths.state, paths.global);
        assert_eq!(paths.memory_roots(), vec![dir.join("global/memory")]);
        fs::remove_dir_all(dir).unwrap();
    }
}
