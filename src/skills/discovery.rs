//! skill 发现：扫描约定的目录位置，产出一份去重后的 [`SkillMeta`] 列表。
//!
//! 发现位置（跨客户端约定，`docs/skills.md` 2.3）：
//!
//! | 层级 | 路径 | 优先级 |
//! | --- | --- | --- |
//! | 项目级 | `<project>/.agents/skills/` | 高 |
//! | 项目级（兼容） | `<project>/.claude/skills/` | 高 |
//! | 用户级 | `~/.agents/skills/` | 低 |
//! | 用户级（兼容） | `~/.claude/skills/` | 低 |
//!
//! **重名规则**：先扫到的赢（项目级天然先于用户级 → 项目覆盖用户）。同一根目录内
//! 按目录名排序扫描，保证结果确定。
//!
//! **容错**：`SKILL.md` 缺失 / 解析失败（缺 `description` 等）→ 跳过该 skill 并
//! 记一条 warn，绝不让一个坏 skill 拖垮整个目录（对齐官方客户端指南第 2 步）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::parse::{parse_skill, SkillMeta};

/// 相对项目根的 skill 目录（按优先级排列，先扫的赢）。
const PROJECT_SKILL_DIRS: &[&str] = &[".agents/skills", ".claude/skills"];
/// 相对用户 home 的 skill 目录（按优先级排列）。
const USER_SKILL_DIRS: &[&str] = &[".agents/skills", ".claude/skills"];
/// skill 定义文件名。
const SKILL_FILE: &str = "SKILL.md";

/// 发现结果：去重后的 skill 列表 + 被覆盖的重名记录（用于观测 / 诊断）。
#[derive(Debug, Default)]
pub struct Discovery {
    pub skills: Vec<SkillMeta>,
    /// 因重名被跳过的 skill：`(name, 保留者根目录, 被跳过者根目录)`。
    pub conflicts: Vec<(String, PathBuf, PathBuf)>,
}

/// 从「项目根 + 用户 home」发现全部 skill。
///
/// `home` 为 `None`（拿不到用户目录）时只扫项目级。
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

/// 按优先级扫描多个根目录，先扫到的名字赢。
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

/// 扫描单个根目录下的**直接子目录**，每个含 `SKILL.md` 的子目录即一个 skill。
///
/// 目录不存在 / 不可读 → 返回空（发现是尽力而为，不报错）。子目录按名字排序，
/// 保证结果确定。
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
        // 跳过隐藏目录（`.git` 等噪音；skill 名本身不允许以 `.` 开头）。
        if dir_name.starts_with('.') {
            continue;
        }
        let skill_file = dir.join(SKILL_FILE);
        let Ok(text) = std::fs::read_to_string(&skill_file) else {
            continue; // 没有 SKILL.md → 不是 skill
        };
        match parse_skill(&text, dir_name, dir.clone()) {
            Ok(meta) => skills.push(meta),
            Err(error) => {
                eprintln!("[skills] 跳过 {}：{error}", skill_file.display());
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
        // 没有 SKILL.md 的目录被跳过
        std::fs::create_dir_all(root.join("not-a-skill")).unwrap();
        // 隐藏目录被跳过
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
        // 项目根下的 .agents/skills
        let proj_skills = project.join(".agents/skills");
        std::fs::create_dir_all(&proj_skills).unwrap();
        write_skill(&proj_skills, "dup", "project version");
        // home 下的 .agents/skills
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
