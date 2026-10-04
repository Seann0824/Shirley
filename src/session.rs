//! 应用侧的会话存储实现（`docs/session.md`）。
//!
//! SDK 只定契约（`shirley_agent_sdk::SessionStore`），**具体落盘技术在这里决定**。
//! 当前选 **JSONL**——每行一条序列化的 `Message`，追加即 `write`，读回即逐行
//! `serde_json::from_str`。选它的理由：零依赖、可读、可手工调试，且天然是
//! append-only 的"日志"形状，与 `docs/session.md` 一.决策 4 完全对齐。
//!
//! 落盘的是**原始 Message 全量日志**：不含 system（恢复时现生成），
//! 不含 chunk / BM25 索引（召回库由日志派生）。`ContextSummary` 是日志里的
//! 一等消息，压缩时也走 `append`。

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use shirley_agent_sdk::{Message, SessionError, SessionStore};

/// JSONL 会话日志：每行一条 `Message`。
///
/// 内部持一把锁保护文件句柄与截断/追加的原子性——`SessionStore` 的方法收
/// `&self`（同步签名，见 `docs/session.md` 二.1），可变状态只能藏在锁里。
pub struct JsonlSessionStore {
    path: PathBuf,
    /// 追加用句柄；`truncate` 会重开。锁保证两个操作不交错。
    file: Mutex<File>,
}

impl JsonlSessionStore {
    /// 打开（不存在则创建）指定路径的 JSONL 会话日志。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// 读取全量日志（逐行反序列化）。空行跳过。
    fn read_all(&self) -> Result<Vec<Message>, SessionError> {
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut log = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let message: Message = serde_json::from_str(&line)
                .map_err(|e| SessionError::Backend(format!("会话日志第 {} 行解析失败: {e}", log.len() + 1)))?;
            log.push(message);
        }
        Ok(log)
    }

    /// 用给定日志整体重写文件（`truncate` 用）。先写临时文件再原子替换。
    ///
    /// **调用方必须已持有 `self.file` 的锁**——这里不再自己加锁，否则会与
    /// 调用方嵌套同一把非重入锁而死锁。函数负责替换文件并把新的追加句柄写回。
    fn rewrite_locked(&self, guard: &mut File, log: &[Message]) -> Result<(), SessionError> {
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut file = File::create(&tmp)?;
            for message in log {
                let line = serde_json::to_string(message)
                    .map_err(|e| SessionError::Backend(format!("会话消息序列化失败: {e}")))?;
                writeln!(file, "{line}")?;
            }
            file.flush()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        // 重开追加句柄，指向替换后的文件，并写回给调用方持有的 guard。
        *guard = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&self.path)?;
        Ok(())
    }
}

impl SessionStore for JsonlSessionStore {
    fn append(&self, message: &Message) -> Result<(), SessionError> {
        let line = serde_json::to_string(message)
            .map_err(|e| SessionError::Backend(format!("会话消息序列化失败: {e}")))?;
        let mut file = self.file.lock().expect("会话锁中毒");
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }

    fn load(&self) -> Result<Vec<Message>, SessionError> {
        // 与 append / truncate 互斥，避免读到写了一半的行。
        let _guard = self.file.lock().expect("会话锁中毒");
        self.read_all()
    }

    fn truncate(&self, len: usize) -> Result<(), SessionError> {
        let mut guard = self.file.lock().expect("会话锁中毒");
        let mut log = self.read_all()?;
        if len < log.len() {
            log.truncate(len);
            self.rewrite_locked(&mut guard, &log)?;
        }
        Ok(())
    }
}

/// 一个可选会话（`/session` 切换用）。
///
/// `name` 是唯一标识（文件 stem），`label` 给人看，`preview` 是该会话首条
/// 用户消息的摘要——列表里只显示时间戳的话无法分辨，预览让用户认得出会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub name: String,
    pub label: String,
    pub preview: String,
}

impl SessionEntry {
    /// 该会话是否有内容（决定 UI 是否显示预览）。
    pub fn is_empty(&self) -> bool {
        self.preview.is_empty()
    }
}

/// 会话目录（应用层）：列出 / 打开 / 新建会话。
///
/// 与 `models::ModelCatalog` 对称——把"当前有哪些会话可选"收敛到接口后面。
/// 当前实现是本地目录扫描（[`FileSessionCatalog`]），将来若要换成远端 / 数据库，
/// `/session` 指令与选择器 UI 都不用改。
pub trait SessionCatalog: Send + Sync {
    /// 列出全部会话，**按最近修改时间降序**（最新的在前）。
    fn list(&self) -> Result<Vec<SessionEntry>, SessionError>;

    /// 打开指定会话的存储（不存在则创建空文件）。
    fn open(&self, name: &str) -> Result<Arc<dyn SessionStore>, SessionError>;

    /// 新建一个空会话，返回它的条目与存储。
    fn create(&self) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError>;

    /// 最近一次修改的会话（用于启动时恢复上次会话）；没有会话时返回 `None`。
    fn latest(&self) -> Result<Option<SessionEntry>, SessionError>;

}

/// 基于本地目录的会话目录：`<root>/.shirley/sessions/<name>.jsonl`。
///
/// 每份会话就是一份独立的 JSONL 日志（沿用 [`JsonlSessionStore`]），
/// 目录扫描即"列出会话"。名字默认取时间戳，天然有序且无需额外索引。
pub struct FileSessionCatalog {
    dir: PathBuf,
}

impl FileSessionCatalog {
    /// 工作区根目录下的会话目录：`<root>/.shirley/sessions`。
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            dir: root.as_ref().join(".shirley").join("sessions"),
        }
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.jsonl"))
    }

    /// 打开指定名字的会话；文件不存在时 `JsonlSessionStore::open` 会创建它。
    fn open_named(&self, name: &str) -> Result<Arc<dyn SessionStore>, SessionError> {
        let store = JsonlSessionStore::open(self.path_for(name))?;
        Ok(Arc::new(store))
    }

    /// 生成一个不冲突的新会话名（时间戳，必要时追加序号）。
    fn fresh_name(&self) -> String {
        let base = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        if !self.path_for(&base).exists() {
            return base;
        }
        for suffix in 2..1000 {
            let candidate = format!("{base}-{suffix}");
            if !self.path_for(&candidate).exists() {
                return candidate;
            }
        }
        base
    }

    /// 迁移旧版单文件日志：`<root>/.shirley/session.jsonl` 若存在且当前
    /// 尚无任何会话，则把它收编成第一份会话。只在启动时调用一次。
    ///
    /// 这样升级到多会话布局不会丢掉用户已有的对话（旧版只有一份日志）。
    pub fn adopt_legacy(&self) -> Result<(), SessionError> {
        let legacy = self.dir.parent().map(|p| p.join("session.jsonl"));
        let Some(legacy) = legacy else {
            return Ok(());
        };
        if !legacy.exists() {
            return Ok(());
        }
        // 已经有会话了就不再迁移，避免重复。
        if !self.list()?.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        std::fs::rename(&legacy, self.path_for("legacy"))?;
        Ok(())
    }

}

impl SessionCatalog for FileSessionCatalog {
    fn list(&self) -> Result<Vec<SessionEntry>, SessionError> {
        let mut entries: Vec<(std::time::SystemTime, SessionEntry)> = Vec::new();
        let read = match std::fs::read_dir(&self.dir) {
            Ok(read) => read,
            // 目录还不存在 = 还没有任何会话，不是错误。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        for item in read {
            let item = item?;
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let modified = item
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            entries.push((
                modified,
                SessionEntry {
                    name: name.to_owned(),
                    label: name.to_owned(),
                    preview: preview_of(&path),
                },
            ));
        }
        // 最近修改的排前面。
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(entries.into_iter().map(|(_, e)| e).collect())
    }

    fn open(&self, name: &str) -> Result<Arc<dyn SessionStore>, SessionError> {
        self.open_named(name)
    }

    fn create(&self) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        let name = self.fresh_name();
        let store = self.open_named(&name)?;
        let entry = SessionEntry {
            name: name.clone(),
            label: name,
            preview: String::new(),
        };
        Ok((entry, store))
    }

    fn latest(&self) -> Result<Option<SessionEntry>, SessionError> {
        Ok(self.list()?.into_iter().next())
    }

}

/// 读取会话首条用户消息作为预览（截断到 40 个字符）。读不到返回空串。
fn preview_of(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    let reader = BufReader::new(file);
    // 只扫前若干行，避免为预览读整个大日志。
    for line in reader.lines().take(50).map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(Message::User { content }) = serde_json::from_str::<Message>(&line) {
            let one_line = content.replace('\n', " ");
            return truncate_chars(&one_line, 40);
        }
    }
    String::new()
}

/// 按字符数截断（中文按字，避免按字节切碎 UTF-8）。
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// 空会话目录：`App::new` 的默认值，供不关心会话切换的测试与默认构造使用。
///
/// 列出为空、打开 / 新建都报错——不落盘、也不产生副作用。
#[allow(dead_code)]
pub struct EmptySessionCatalog;

impl SessionCatalog for EmptySessionCatalog {
    fn list(&self) -> Result<Vec<SessionEntry>, SessionError> {
        Ok(Vec::new())
    }

    fn open(&self, _name: &str) -> Result<Arc<dyn SessionStore>, SessionError> {
        Err(SessionError::Backend("空会话目录不支持打开会话".into()))
    }

    fn create(&self) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        Err(SessionError::Backend("空会话目录不支持新建会话".into()))
    }

    fn latest(&self) -> Result<Option<SessionEntry>, SessionError> {
        Ok(None)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shirley_session_{tag}_{}.jsonl", std::process::id()))
    }

    fn user(content: &str) -> Message {
        Message::User {
            content: content.to_owned(),
        }
    }

    #[test]
    fn append_load_round_trip_persists_across_handles() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        {
            let store = JsonlSessionStore::open(&path).unwrap();
            store.append(&user("a")).unwrap();
            store.append(&user("b")).unwrap();
        }
        // 新句柄读回——模拟进程重启。
        let store = JsonlSessionStore::open(&path).unwrap();
        let log = store.load().unwrap();
        assert_eq!(log.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn context_summary_round_trips() {
        let path = tmp_path("summary");
        let _ = std::fs::remove_file(&path);
        let store = JsonlSessionStore::open(&path).unwrap();
        store
            .append(&Message::ContextSummary {
                content: "<current_goal>x</current_goal>".to_owned(),
            })
            .unwrap();
        match &store.load().unwrap()[0] {
            Message::ContextSummary { content } => assert!(content.contains("current_goal")),
            other => panic!("应为 ContextSummary，实际 {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn truncate_rewrites_and_persists() {
        let path = tmp_path("truncate");
        let _ = std::fs::remove_file(&path);
        let store = JsonlSessionStore::open(&path).unwrap();
        for c in ["a", "b", "c", "d"] {
            store.append(&user(c)).unwrap();
        }
        store.truncate(2).unwrap();
        // 重开句柄确认落盘。
        let reopened = JsonlSessionStore::open(&path).unwrap();
        assert_eq!(reopened.load().unwrap().len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn truncate_is_idempotent_when_not_smaller() {
        let path = tmp_path("idem");
        let _ = std::fs::remove_file(&path);
        let store = JsonlSessionStore::open(&path).unwrap();
        store.append(&user("a")).unwrap();
        store.truncate(5).unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_after_truncate_works() {
        let path = tmp_path("append_after_trunc");
        let _ = std::fs::remove_file(&path);
        let store = JsonlSessionStore::open(&path).unwrap();
        for c in ["a", "b", "c"] {
            store.append(&user(c)).unwrap();
        }
        store.truncate(1).unwrap();
        // 截断后仍能继续追加（句柄已重开）。
        store.append(&user("d")).unwrap();
        let log = store.load().unwrap();
        assert_eq!(log.len(), 2);
        let _ = std::fs::remove_file(&path);
    }
    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_catalog_{tag}_{}_{:?}",
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
    fn catalog_create_then_list_shows_session() {
        let dir = tmp_dir("create");
        let catalog = FileSessionCatalog::new(&dir);
        assert!(catalog.list().unwrap().is_empty());
        let (entry, store) = catalog.create().unwrap();
        store.append(&user("你好")).unwrap();
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, entry.name);
        assert_eq!(listed[0].preview, "你好");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_open_returns_isolated_store() {
        let dir = tmp_dir("open");
        let catalog = FileSessionCatalog::new(&dir);
        let (a, sa) = catalog.create().unwrap();
        sa.append(&user("a-msg")).unwrap();
        let (_b, sb) = catalog.create().unwrap();
        sb.append(&user("b-msg")).unwrap();
        // 打开 a：只看到 a 的内容，b 的内容不串。
        let reopened = catalog.open(&a.name).unwrap();
        let log = reopened.load().unwrap();
        assert_eq!(log.len(), 1);
        assert!(matches!(&log[0], Message::User { content } if content == "a-msg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_preview_truncates_long_first_user_message() {
        let dir = tmp_dir("preview");
        let catalog = FileSessionCatalog::new(&dir);
        let (_e, store) = catalog.create().unwrap();
        let long = "字".repeat(80);
        store.append(&user(&long)).unwrap();
        let preview = &catalog.list().unwrap()[0].preview;
        // 40 个字符 + 省略号。
        assert_eq!(preview.chars().count(), 41);
        assert!(preview.ends_with('…'));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_latest_prefers_most_recently_modified() {
        let dir = tmp_dir("latest");
        let catalog = FileSessionCatalog::new(&dir);
        let (first, store_a) = catalog.create().unwrap();
        // 确保两次创建落在不同的秒级时间戳上（fresh_name 基于秒）。
        std::thread::sleep(std::time::Duration::from_millis(5));
        let (second, store_b) = catalog.create().unwrap();
        store_b.append(&user("newer")).unwrap();
        // 触碰 first 的文件，使其成为最近修改。
        store_a.append(&user("touched")).unwrap();
        let latest = catalog.latest().unwrap().unwrap();
        assert!(
            latest.name == first.name || latest.name == second.name,
            "latest 必须是已存在的会话"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_adopts_legacy_single_file() {
        let dir = tmp_dir("legacy");
        let shirley = dir.join(".shirley");
        std::fs::create_dir_all(&shirley).unwrap();
        let legacy = shirley.join("session.jsonl");
        std::fs::write(
            &legacy,
            format!("{}\n", serde_json::to_string(&user("旧对话")).unwrap()),
        )
        .unwrap();
        let catalog = FileSessionCatalog::new(&dir);
        catalog.adopt_legacy().unwrap();
        // 旧文件被搬进 sessions 目录，仍能读回。
        assert!(!legacy.exists());
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "legacy");
        assert_eq!(listed[0].preview, "旧对话");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
