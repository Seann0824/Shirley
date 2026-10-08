//! 工作区文件检索——供桌面界面的 `@` 引用使用。
//!
//! 刻意放在应用层、**不碰 SDK**：这只是"列出工作区里有哪些文件、按关键词排个序"，
//! 与 Agent 运行时无关。SDK 没有为它新增任何对外能力（这正是"做 desktop 不必改
//! SDK"的一个注脚——能用应用层解决的，就别往 SDK 里塞）。
//!
//! 边界与 `read_file` 工具一致：只在工作区根目录下遍历，跳过依赖/产物/版本控制
//! 目录，并对遍历规模设上限，避免在大仓库里把 UI 卡住。

#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

use std::path::Path;

/// 遍历时跳过的目录名（依赖、产物、VCS、Shirley 自身状态）。
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

/// 单次遍历访问的条目上限（防大仓库卡 UI）。
const MAX_VISITED: usize = 20_000;
/// 返回结果上限。
const DEFAULT_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// 相对工作区根的路径，统一用 `/` 分隔（跨平台一致，便于前端展示与插入）。
    pub path: String,
    /// 末段名字（用于 chip 展示）。
    pub name: String,
    pub kind: FileKind,
}

/// 在工作区根目录下按 `query` 检索文件/目录。
///
/// - `query` 为空：返回广度优先的前 `limit` 个条目（近似"最近/顶层优先"）。
/// - `query` 非空：大小写不敏感匹配，按匹配质量排序（前缀 > 名字子串 > 路径子串 >
///   子序列），命中越多、路径越短越靠前。
///
/// 只做只读遍历，出错（权限等）静默跳过，尽力而为。
pub fn search_files(root: &Path, query: &str, limit: Option<usize>) -> Vec<FileEntry> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).max(1);
    let query = query.trim().to_lowercase();

    let mut entries: Vec<FileEntry> = Vec::new();
    let mut visited = 0usize;

    // 广度优先：用 VecDeque 保证顶层/浅层优先。
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
                // 工作区根目录下的隐藏条目（.git / .env 等）不参与引用。
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

        // 目录与文件混排时按名字排序，保证结果稳定（也利于缓存/可预期）。
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

/// `needle` 是否为 `haystack` 的子序列（大小写已归一）。
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
