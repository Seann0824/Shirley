//! 记忆的持久化层：Markdown 目录的读写（`docs/memory.md` §3.1）。
//!
//! 布局（两级：全局个人记忆 + 工作区项目记忆）：
//!
//! ```text
//! <config_dir>/shirley/memory/   # 全局（跨项目）
//! ├── core.md                    # 常驻层：画像 + 明确偏好 + 活跃项目
//! ├── index.md                   # 程序维护的索引页（见 index.rs）
//! ├── episodic/                  # 情景记忆（事件）
//! ├── preferences/               # 偏好 / 习惯
//! ├── procedures/                # 程序记忆（行为流程）
//! └── facts/                     # 事实
//! <root>/.shirley/memory/        # 工作区级（随仓库走）
//! ```
//!
//! **写只落主根**（`roots[0]`，通常是全局）；**读合并所有根**，同名 `id` 后出现的
//! 覆盖先出现的（工作区覆盖全局）。用同步 `std::fs`：记忆目录很小，且
//! `ContextProvider::context()` 是同步接口，读必须同步。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::format::{Entry, EntryType, MemoryError};

/// 常驻概览文件名。
pub const CORE_FILE: &str = "core.md";
/// 索引页文件名。
pub const INDEX_FILE: &str = "index.md";

/// 条目子目录（`type` → 目录，见 [`dir_for`]）。
pub const ENTRY_DIRS: [&str; 4] = ["episodic", "preferences", "procedures", "facts"];

/// 记忆存储：持有若干根目录（第一个为写入主根）。
pub struct MemoryStore {
    roots: Vec<PathBuf>,
}

impl MemoryStore {
    /// 单根存储（测试 / 工作区级）。
    ///
    /// V1 生产路径统一走 [`MemoryStore::with_roots`]（全局 + 工作区双根），
    /// 本构造仅测试与未来"单根工作区"场景使用。
    #[allow(dead_code)]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            roots: vec![root.into()],
        }
    }

    /// 多根存储（全局 + 工作区）。`roots[0]` 为写入主根。
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        assert!(!roots.is_empty(), "memory store needs at least one root");
        Self { roots }
    }

    /// 写入主根。
    pub fn primary_root(&self) -> &Path {
        &self.roots[0]
    }

    /// 建好全部条目子目录（幂等）。`core.md` / `index.md` 由写入方按需创建。
    pub fn ensure_layout(&self) -> Result<(), MemoryError> {
        let root = self.primary_root();
        std::fs::create_dir_all(root)?;
        for dir in ENTRY_DIRS {
            std::fs::create_dir_all(root.join(dir))?;
        }
        Ok(())
    }

    /// 读常驻概览 `core.md`（多根合并：工作区内容追加在全局之后）。空 / 缺失返回 `None`。
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

    /// 写常驻概览 `core.md`（落主根）。
    ///
    /// V1 的 `core.md` 由用户 / 应用显式维护，curator 不写它；本方法供测试与
    /// 未来"自动维护 core"使用。
    #[allow(dead_code)]
    pub fn write_core(&self, text: &str) -> Result<(), MemoryError> {
        self.ensure_layout()?;
        std::fs::write(self.primary_root().join(CORE_FILE), text)?;
        Ok(())
    }

    /// 一条条目应落的绝对路径：`<primary>/<dir>/<id>.md`。
    pub fn entry_path(&self, entry: &Entry) -> PathBuf {
        self.primary_root()
            .join(dir_for(entry.entry_type))
            .join(format!("{}.md", entry.id))
    }

    /// 写入一条条目（落主根，`id` 决定文件名）。返回写入路径。
    pub fn write_entry(&self, entry: &Entry) -> Result<PathBuf, MemoryError> {
        self.ensure_layout()?;
        let path = self.entry_path(entry);
        std::fs::write(&path, entry.render())?;
        Ok(path)
    }

    /// 读一条条目。
    #[allow(dead_code)]
    pub fn read_entry(&self, path: &Path) -> Result<Entry, MemoryError> {
        let text = std::fs::read_to_string(path)?;
        Entry::parse(&text)
    }

    /// 遍历所有根、所有条目目录，解析出全部条目，按 `id` 升序返回。
    ///
    /// 同名 `id` 后出现者覆盖先出现者（工作区覆盖全局）。解析失败的条目**跳过**
    /// （不让一条坏文件拖垮整库），返回 `(路径, 条目)`。
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

    /// 把绝对路径表达成相对某个根的展示路径（索引页用）。
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

    /// 删一条条目（按 `id` 在所有根里找，找到即删）。返回是否删到。
    ///
    /// `delete` 是 curator 的动作之一（候选被环境探测证伪时）。
    ///
    /// V1 curator 只做 upsert + supersede，暂不接线删除；保留为 store 契约。
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

/// `type` → 条目子目录。
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
    use crate::memory::format::{Confidence, EntryStatus};

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_mem_store_{tag}_{}_{}",
            std::process::id(),
            // 让同一进程内多次调用拿到不同目录。
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
        let original = entry("pref-a", EntryType::Preference, "偏好 A。");
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
        store.write_core("用户是 Sean。").unwrap();
        assert_eq!(store.read_core().as_deref(), Some("用户是 Sean。"));
    }

    #[test]
    fn workspace_root_overrides_global_for_same_id() {
        let global = tmp("global");
        let workspace = tmp("workspace");
        let g = MemoryStore::new(&global);
        let w = MemoryStore::new(&workspace);
        g.write_entry(&entry("fact-db", EntryType::Fact, "全局：用 MySQL。"))
            .unwrap();
        w.write_entry(&entry("fact-db", EntryType::Fact, "工作区：用 Postgres。"))
            .unwrap();

        // roots[0]=全局（写），roots[1]=工作区（读时覆盖）。
        let merged = MemoryStore::with_roots(vec![global.clone(), workspace.clone()]);
        let listed = merged.list_entries().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.body, "工作区：用 Postgres。");
    }

    #[test]
    fn core_merges_global_then_workspace() {
        let global = tmp("core_g");
        let workspace = tmp("core_w");
        MemoryStore::new(&global).write_core("全局：用户是 Sean。").unwrap();
        MemoryStore::new(&workspace).write_core("工作区：正在迁移。").unwrap();
        let merged = MemoryStore::with_roots(vec![global, workspace]);
        assert_eq!(
            merged.read_core().as_deref(),
            Some("全局：用户是 Sean。\n\n工作区：正在迁移。")
        );
    }

    #[test]
    fn malformed_entry_is_skipped() {
        let root = tmp("malformed");
        let store = MemoryStore::new(&root);
        store.ensure_layout().unwrap();
        std::fs::write(root.join("facts/broken.md"), "not a valid entry").unwrap();
        store
            .write_entry(&entry("fact-ok", EntryType::Fact, "好的。"))
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
            .write_entry(&entry("fact-gone", EntryType::Fact, "删我。"))
            .unwrap();
        assert!(store.delete_entry("fact-gone").unwrap());
        assert!(store.list_entries().unwrap().is_empty());
        assert!(!store.delete_entry("fact-gone").unwrap());
    }
}
