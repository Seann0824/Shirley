//! 应用侧的项目注册表（多工作区 / 多项目）。
//!
//! 桌面界面允许用户把**多个工作区**登记为「项目」，每个项目下挂自己的会话。
//! 本模块只管一件事：**哪些目录是项目**——一份跨工作区的全局注册表，
//! 落 `<config_dir>/shirley/projects.json`（与全局记忆同根，见 `docs/memory.md`）。
//!
//! **这是应用层的事，不进 SDK**：SDK 只认单个 `working_dir`；「我同时开着哪些
//! 项目」是桌面界面的编排，不是基础能力。
//!
//! 会话日志仍落**各项目自己的** `<root>/.shirley/sessions/`（随仓库走，可迁移、
//! 可 gitignore）——注册表只记「项目在哪」，不搬动项目内的数据。
//!
//! # 归档语义
//!
//! 「移除项目」= **归档**：只置 `archived = true`，**不删磁盘上的任何东西**
//! （会话、记忆、仓库都原样保留）。归档的项目默认不出现在侧栏，可从归档列表
//! **恢复**。这样误操作可逆——用户抱怨过「移除」太吓人。
//!
//! # 身份
//!
//! 项目 `id` = 目录的**规范路径**（canonicalize 后）的字符串形式。同一目录被
//! 反复添加时按 id 去重（若此前已归档，则**恢复并改名**，而非新增一条）。

// 桌面专属模块：默认（TUI-only）构建里没有消费者，`dead_code` 是预期噪音。
#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 全局注册表相对 `config_dir` 的位置（与全局记忆 `shirley/memory` 同根）。
const REGISTRY_REL: &str = "shirley/projects.json";

/// 注册表错误。`Io` 覆盖读写失败，`Canonicalize` 覆盖路径无效（不存在 / 无权限）。
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("项目注册表读写失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("项目注册表解析失败: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("路径无效: {0}")]
    Canonicalize(String),
    #[error("项目不存在: {0}")]
    NotFound(String),
}

/// 一个已登记的项目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectEntry {
    /// 规范路径字符串——项目的稳定身份（去重键）。
    pub id: String,
    /// 展示名（可改名）。
    pub name: String,
    /// 工作区根目录。
    pub path: PathBuf,
    /// 是否已归档（归档的项目默认不显示，可恢复）。
    #[serde(default)]
    pub archived: bool,
    /// 首次登记时间（毫秒）。
    #[serde(default)]
    pub created_ms: u64,
    /// 最近打开时间（毫秒）——列表按它降序。
    #[serde(default)]
    pub last_opened_ms: u64,
}

/// 磁盘上的注册表结构（版本化，便于将来迁移）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    projects: Vec<ProjectEntry>,
}

/// 当前注册表文件版本。
const REGISTRY_VERSION: u32 = 1;

/// 项目注册表：内存态 + 落盘路径。
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

/// 目录名兜底当展示名（规范化失败 / 无文件名时用完整路径）。
fn default_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

impl ProjectRegistry {
    /// 使用真实的 `config_dir` 载入（拿不到 `config_dir` 时退化为不落盘的空表）。
    pub fn load_default() -> Result<Self, ProjectError> {
        let path = dirs::config_dir()
            .map(|dir| dir.join(REGISTRY_REL))
            .unwrap_or_else(|| PathBuf::from("shirley-projects.json"));
        Self::load(path)
    }

    /// 从指定路径载入（文件不存在 = 空表）。测试用注入路径。
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

    /// 落盘（先建目录、写临时文件再原子替换，避免写一半损坏）。
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

    /// 列出项目（按最近打开降序）。`include_archived = false` 时隐藏归档项目。
    pub fn list(&self, include_archived: bool) -> Vec<&ProjectEntry> {
        let mut entries: Vec<&ProjectEntry> = self
            .projects
            .iter()
            .filter(|entry| include_archived || !entry.archived)
            .collect();
        entries.sort_by(|a, b| b.last_opened_ms.cmp(&a.last_opened_ms));
        entries
    }

    /// 按 id 取项目。
    pub fn get(&self, id: &str) -> Option<&ProjectEntry> {
        self.projects.iter().find(|entry| entry.id == id)
    }

    /// 登记一个项目（按规范路径去重）。
    ///
    /// - 目录此前未登记 → 新增一条。
    /// - 已登记但**已归档** → 恢复它（`archived = false`），并用 `name` 覆盖展示名。
    /// - 已登记且未归档 → 只刷新 `last_opened_ms`（不重复登记）。
    ///
    /// 返回登记后的条目（克隆）。
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

    /// 改名（只改展示名，不动路径 / 会话数据）。
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

    /// 归档（不删磁盘数据，只置标志）。
    pub fn archive(&mut self, id: &str) -> Result<(), ProjectError> {
        let entry = self
            .projects
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| ProjectError::NotFound(id.to_owned()))?;
        entry.archived = true;
        self.save()
    }

    /// 从归档恢复，并刷新最近打开时间。
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

    /// 刷新最近打开时间（打开项目时调，best-effort 落盘）。
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
        let eb = reg.add(b.clone(), Some("我的项目".into())).unwrap();
        assert_eq!(ea.name, "alpha");
        assert_eq!(eb.name, "我的项目");

        let listed = reg.list(false);
        assert_eq!(listed.len(), 2);
        // 两个 id 都在（顺序是 UI 关注点，见下面的不变量断言）。
        assert!(listed.iter().any(|entry| entry.id == ea.id));
        assert!(listed.iter().any(|entry| entry.id == eb.id));
        // 不变量：列表按 last_opened_ms 降序（同毫秒并列可接受）。
        for pair in listed.windows(2) {
            assert!(pair[0].last_opened_ms >= pair[1].last_opened_ms);
        }

        // 重新载入能读回。
        let reloaded = ProjectRegistry::load(reg.path.clone()).unwrap();
        assert_eq!(reloaded.list(false).len(), 2);
    }

    #[test]
    fn add_is_idempotent_and_dedupes_by_canonical_path() {
        let (root, mut reg) = temp_registry("dedupe");
        let a = make_dir(&root, "alpha");
        let first = reg.add(a.clone(), None).unwrap();
        // 同一目录（含 ./ 变体）再添加：不新增，只刷新。
        let again = reg.add(a.join("."), Some("改名了".into())).unwrap();
        assert_eq!(first.id, again.id);
        assert_eq!(reg.list(false).len(), 1);
        assert_eq!(again.name, "改名了");
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
        reg.rename(&entry.id, "新名字").unwrap();
        let got = reg.get(&entry.id).unwrap();
        assert_eq!(got.name, "新名字");
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
