//! Workspace file search — used by the desktop interface's `@` references.
//!
//! Deliberately in the app layer, **not touching the SDK**: this is just "list what files
//! exist in the workspace and rank them by keyword", unrelated to the agent runtime. The SDK
//! gained no public capability for it (a footnote to "building the desktop doesn't require changing the SDK" — if the app layer can solve it, don't stuff it into the SDK).
//!
//! Its boundary matches the `read` tool: it only walks under the workspace root, skips
//! dependency / build / VCS directories, and caps the traversal size so a big repo can't freeze the UI.

#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

use std::path::Path;

/// Directory names skipped during traversal (dependencies, build output, VCS, Shirley's own state).
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    ".shirley",
    ".next",
    ".venv",
    "venv",
    "__pycache__",
];

/// Max entries visited per traversal (prevents freezing the UI on a big repo).
const MAX_VISITED: usize = 20_000;
/// Max results returned.
const DEFAULT_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Path relative to the workspace root, always `/`-separated (consistent across platforms, easy for the frontend to show and insert).
    pub path: String,
    /// Last path segment (used for chip display).
    pub name: String,
    pub kind: FileKind,
}

/// Search files/directories under the workspace root by `query`.
///
/// - Empty `query`: returns the first `limit` entries in breadth-first order (approximating "nearest / top-level first").
/// - Non-empty `query`: case-insensitive match, ranked by match quality (prefix > name substring > path substring >
///   subsequence); more matches and shorter paths rank higher.
///
/// Read-only traversal only; errors (permissions, etc.) are silently skipped, best-effort.
pub fn search_files(root: &Path, query: &str, limit: Option<usize>) -> Vec<FileEntry> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).max(1);
    let query = query.trim().to_lowercase();

    let mut entries: Vec<FileEntry> = Vec::new();
    let mut visited = 0usize;

    // Breadth-first: use a VecDeque to favor top-level / shallow entries.
    let mut queue: std::collections::VecDeque<(std::path::PathBuf, String)> =
        std::collections::VecDeque::new();
    queue.push_back((root.to_path_buf(), String::new()));

    while let Some((abs, rel)) = queue.pop_front() {
        if visited >= MAX_VISITED {
            break;
        }
        let read = match std::fs::read_dir(&abs) {
            Ok(read) => read,
            Err(_) => continue,
        };

        let mut children: Vec<(std::path::PathBuf, String, bool)> = Vec::new();
        for entry in read.flatten() {
            visited += 1;
            if visited >= MAX_VISITED {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') && rel.is_empty() {
                // Hidden entries at the workspace root (.git / .env, etc.) don't participate in references.
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let is_dir = file_type.is_dir();
            if is_dir && SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            children.push((entry.path(), child_rel, is_dir));
        }

        // Sort by name when mixing dirs and files, for stable results (also helps caching / predictability).
        children.sort_by(|a, b| a.1.cmp(&b.1));

        for (child_abs, child_rel, is_dir) in children {
            if is_dir {
                queue.push_back((child_abs, child_rel.clone()));
            }
            entries.push(FileEntry {
                name: child_rel.rsplit('/').next().unwrap_or(&child_rel).to_string(),
                path: child_rel,
                kind: if is_dir { FileKind::Dir } else { FileKind::File },
            });
        }
    }

    if query.is_empty() {
        entries.truncate(limit);
        return entries;
    }

    let mut scored: Vec<(u8, usize, FileEntry)> = entries
        .into_iter()
        .filter_map(|entry| {
            let path_lower = entry.path.to_lowercase();
            let name_lower = entry.name.to_lowercase();
            let score = if name_lower.starts_with(&query) {
                0
            } else if name_lower.contains(&query) {
                1
            } else if path_lower.contains(&query) {
                2
            } else if is_subsequence(&query, &path_lower) {
                3
            } else {
                return None;
            };
            Some((score, entry.path.len(), entry))
        })
        .collect();

    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.path.cmp(&b.2.path)));
    scored.into_iter().take(limit).map(|(_, _, entry)| entry).collect()
}

/// Whether `needle` is a subsequence of `haystack` (case already normalized).
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|n| chars.any(|h| h == n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "shirley-ws-search-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join("src/interface")).unwrap();
        std::fs::create_dir_all(base.join("node_modules/pkg")).unwrap();
        std::fs::write(base.join("README.md"), "x").unwrap();
        std::fs::write(base.join("src/main.rs"), "x").unwrap();
        std::fs::write(base.join("src/interface/app.rs"), "x").unwrap();
        std::fs::write(base.join("node_modules/pkg/index.js"), "x").unwrap();
        std::fs::write(base.join(".env"), "secret").unwrap();
        base
    }

    #[test]
    fn skips_dependency_and_hidden_dirs() {
        let root = temp_tree();
        let all = search_files(&root, "", Some(100));
        let paths: Vec<_> = all.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"README.md"));
        assert!(paths.contains(&"src/main.rs"));
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
        assert!(!paths.contains(&".env"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn query_ranks_name_prefix_first() {
        let root = temp_tree();
        let hits = search_files(&root, "main", Some(10));
        assert_eq!(hits.first().map(|e| e.path.as_str()), Some("src/main.rs"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn empty_query_is_bounded() {
        let root = temp_tree();
        let hits = search_files(&root, "", Some(2));
        assert_eq!(hits.len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn subsequence_matches_path() {
        let root = temp_tree();
        let hits = search_files(&root, "intapp", Some(10));
        assert!(hits.iter().any(|e| e.path == "src/interface/app.rs"));
        let _ = std::fs::remove_dir_all(root);
    }
}
