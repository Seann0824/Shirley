//! skill discovery: scan the conventional directory locations, producing a deduplicated list of [`SkillMeta`].
//!
//! Discovery locations (cross-client convention, `docs/skills.md` 2.3):
//!
//! | Level | Path | Priority |
//! | --- | --- | --- |
//! | Project | `<project>/.agents/skills/` | high |
//! | Project (compatible) | `<project>/.claude/skills/` | high |
//! | User | `~/.agents/skills/` | low |
//! | User (compatible) | `~/.claude/skills/` | low |
//!
//! **Duplicate-name rule**: first scanned wins (project level naturally precedes user level -> project overrides user). Within one root
//! scan in directory-name order to keep the result deterministic.
//!
//! **Leniency**: missing `SKILL.md` / parse failure (missing `description`, etc.) -> skip that skill and
//! log a warn; never let one bad skill bring down the whole directory (aligned with step 2 of the official client guide).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::parse::{SkillMeta, parse_skill};

/// skill directories relative to the project root (in priority order, first scanned wins).
const PROJECT_SKILL_DIRS: &[&str] = &[".agents/skills", ".claude/skills"];
/// skill directories relative to the user home (in priority order).
const USER_SKILL_DIRS: &[&str] = &[".agents/skills", ".claude/skills"];
/// skill definition file name.
const SKILL_FILE: &str = "SKILL.md";

/// Discovery result: the deduplicated skill list + overridden duplicate-name records (for observation / diagnostics).
#[derive(Debug, Default)]
pub struct Discovery {
    pub skills: Vec<SkillMeta>,
    /// skills skipped due to duplicate names: `(name, keeper root dir, skipped root dir)`.
    pub conflicts: Vec<(String, PathBuf, PathBuf)>,
}

/// Discover all skills from "project root + user home".
///
/// When `home` is `None` (user directory unavailable), only scan project level.
pub fn discover(project_root: &Path, home: Option<&Path>) -> Discovery {
    let mut roots: Vec<PathBuf> = PROJECT_SKILL_DIRS
        .iter()
        .map(|rel| project_root.join(rel))
        .collect();
    if let Some(home) = home {
        roots.extend(USER_SKILL_DIRS.iter().map(|rel| home.join(rel)));
    }
    scan_roots(&roots)
}

/// Scan multiple root directories by priority; the first scanned name wins.
pub fn scan_roots(roots: &[PathBuf]) -> Discovery {
    let mut discovery = Discovery::default();
    let mut seen: HashSet<String> = HashSet::new();

    for root in roots {
        for meta in scan_root(root) {
            if seen.contains(&meta.name) {
                let kept = discovery
                    .skills
                    .iter()
                    .find(|s| s.name == meta.name)
                    .map(|s| s.root.clone())
                    .unwrap_or_default();
                discovery.conflicts.push((meta.name, kept, meta.root));
                continue;
            }
            seen.insert(meta.name.clone());
            discovery.skills.push(meta);
        }
    }
    discovery
}

/// Scan the **direct subdirectories** of a single root directory; each subdirectory containing a `SKILL.md` is a skill.
///
/// Directory missing / unreadable -> returns empty (discovery is best-effort, no error). Subdirectories are sorted by name,
/// keeping the result deterministic.
fn scan_root(root: &Path) -> Vec<SkillMeta> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();

    let mut skills = Vec::new();
    for dir in dirs {
        let Some(dir_name) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        // skip hidden directories (noise like `.git`; skill names themselves may not start with `.`).
        if dir_name.starts_with('.') {
            continue;
        }
        let skill_file = dir.join(SKILL_FILE);
        let Ok(text) = std::fs::read_to_string(&skill_file) else {
            continue; // no SKILL.md -> not a skill
        };
        match parse_skill(&text, dir_name, dir.clone()) {
            Ok(meta) => skills.push(meta),
            Err(error) => {
                eprintln!("[skills] skipping {}: {error}", skill_file.display());
            }
        }
    }
    skills
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, description: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(SKILL_FILE),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nbody\n"),
        )
        .unwrap();
    }

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_skills_{tag}_{}_{}",
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
    fn scans_direct_subdirs_with_skill_file() {
        let root = tmp("scan");
        write_skill(&root, "alpha", "does alpha");
        write_skill(&root, "beta", "does beta");
        // directories without SKILL.md are skipped
        std::fs::create_dir_all(root.join("not-a-skill")).unwrap();
        // hidden directories are skipped
        write_skill(&root, ".hidden", "should be skipped");

        let found = scan_root(&root);
        let names: Vec<_> = found.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn project_overrides_user_on_conflict() {
        let project = tmp("proj");
        let home = tmp("home");
        // .agents/skills under the project root
        let proj_skills = project.join(".agents/skills");
        std::fs::create_dir_all(&proj_skills).unwrap();
        write_skill(&proj_skills, "dup", "project version");
        // .agents/skills under home
        let home_skills = home.join(".agents/skills");
        std::fs::create_dir_all(&home_skills).unwrap();
        write_skill(&home_skills, "dup", "user version");
        write_skill(&home_skills, "user-only", "only in user");

        let discovery = discover(&project, Some(&home));
        let dup = discovery.skills.iter().find(|s| s.name == "dup").unwrap();
        assert_eq!(dup.description, "project version");
        assert!(discovery.skills.iter().any(|s| s.name == "user-only"));
        assert_eq!(discovery.conflicts.len(), 1);
        assert_eq!(discovery.conflicts[0].0, "dup");

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn missing_root_is_not_an_error() {
        let discovery = discover(Path::new("/nonexistent/shirley/skills"), None);
        assert!(discovery.skills.is_empty());
    }

    #[test]
    fn bad_skill_is_skipped_not_fatal() {
        let root = tmp("bad");
        write_skill(&root, "good", "ok");
        let bad = root.join("broken");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join(SKILL_FILE), "no frontmatter here").unwrap();

        let found = scan_root(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "good");
        std::fs::remove_dir_all(&root).ok();
    }
}
