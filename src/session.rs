//! 应用侧的会话存储（`docs/session.md`）。
//!
//! 会话持久化**完全属于应用层**：契约（[`SessionStore`] / [`SessionError`]）与
//! 实现都在这里，SDK 不再持有任何会话概念。当前落盘选 **JSONL**——每行一条序列化
//! 的 `Message`，追加即 `write`，读回即逐行 `serde_json::from_str`。选它的理由：
//! 零依赖、可读、可手工调试，且天然是 append-only 的"日志"形状，与
//! `docs/session.md` 一.决策 4 完全对齐。
//!
//! 落盘的是**原始 Message 全量日志**：不含 system（恢复时现生成）。
//! `ContextSummary` 是日志里的一等消息，压缩时也走 `append`。
//!
//! 落库时机由应用层驱动：`Session::apply_event` 收到 SDK 的 `MessageAdded` 事件即
//! 调 [`SessionStore::append`]（SDK 只发事件、不落库）。

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use shirley_agent_sdk::Message;

/// 会话日志的存储抽象。
///
/// 三个方法对应三种操作，**恰好是会话写入的全部语义**：追加（正常轮次 + 压缩）、
/// 读回（恢复）、截尾（rewind）。故意没有 `delete` / `update` / `search`——用不到。
///
/// **签名是同步的**：`SessionManager` 的恢复 / 截尾发生在同步路径上，同步接口让
/// 调用方不必变异步。存储后端若是异步驱动，由实现内部消化。契约同步，实现自由。
pub trait SessionStore: Send + Sync {
    /// 追加一条消息到日志尾部。压缩产生的 `ContextSummary` 也走这里。
    fn append(&self, message: &Message) -> Result<(), SessionError>;

    /// 读取全量日志（只读、顺序）。
    fn load(&self) -> Result<Vec<Message>, SessionError>;

    /// 截断到前 `len` 条（rewind 用；只截尾，不产生中间空洞）。
    ///
    /// `len` 不小于当前长度时不做任何事（幂等，避免越界）。
    fn truncate(&self, len: usize) -> Result<(), SessionError>;
}

/// 会话存储失败。
///
/// 展示格式统一为 `[前缀]: 详情`，与 SDK 各层错误（`AdapterError` / `ToolError` /
/// `SandboxError` / `WorkspaceError`）一致；前缀留在变体旁。
///
/// 持久化失败当前一律**当作错误**（`docs/session.md` 二.2）：持久化失败还继续跑，
/// 等于假装有存档，比直接报错更危险。
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// 底层存储 I/O 失败（写盘、读盘、序列化等）。
    #[error("[session storage failure]: {0}")]
    Io(#[from] std::io::Error),

    /// 存储后端本身报告的错误（例如 SQL 失败），带自由文本详情。
    #[error("[session storage error]: {0}")]
    Backend(String),
}

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

/// 惰性会话日志：**启动时不落盘**，首次 `append`（即首条消息）时才创建文件。
///
/// 动机：Codex 的 UX——打开应用时并不产生一个空会话文件，只有真正发消息
/// （产生交互）才建会话。这样会话列表不会被一堆"点了没聊"的空文件塞满。
///
/// 语义上它就是一个 [`SessionStore`]：`Session` 照常持有它，发送路径无需感知
/// "延迟创建"这回事。区别只有两点：
/// - 名字在**构造时**就确定（供 UI 页脚 / 当前会话标记用），但**文件不建**；
/// - 首次 `append` 时才**惰性物化**：按已定的名字落盘 JSONL（并写标题 sidecar）。
///
/// 物化之前，`load` / `truncate` 一律视为空日志（没有文件就没有内容）；
/// 物化后永久持有内部 [`JsonlSessionStore`]，行为与直接打开一份会话完全一致。
///
/// 标题若给出，在物化时落成 sidecar `<name>.title`（与
/// [`FileSessionCatalog::create_named`] 同语义）。
pub struct LazySessionStore {
    catalog: Arc<FileSessionCatalog>,
    /// 启动时已确定的名字（`list` 在物化前扫不到它，因为没有文件）。
    name: String,
    /// 物化后的底层存储（首次 `append` 时创建）。
    inner: Mutex<Option<Arc<JsonlSessionStore>>>,
    /// 可选标题（物化时落成 sidecar）。
    title: Option<String>,
}

impl LazySessionStore {
    fn new(catalog: Arc<FileSessionCatalog>, name: String, title: Option<String>) -> Self {
        Self {
            catalog,
            name,
            inner: Mutex::new(None),
            title,
        }
    }

    /// 物化：按已定的名字落盘空日志、写标题 sidecar，返回底层存储。幂等。
    fn materialize(&self) -> Result<Arc<JsonlSessionStore>, SessionError> {
        let mut inner = self.inner.lock().expect("会话锁中毒");
        if let Some(store) = inner.as_ref() {
            return Ok(store.clone());
        }
        let store = Arc::new(JsonlSessionStore::open(self.catalog.path_for(&self.name))?);
        if let Some(title) = self.title.as_deref() {
            self.catalog.write_title(&self.name, title)?;
        }
        *inner = Some(store.clone());
        Ok(store)
    }
}

impl SessionStore for LazySessionStore {
    fn append(&self, message: &Message) -> Result<(), SessionError> {
        // 首次 append 触发物化；此后直接委托。
        self.materialize()?.append(message)
    }

    fn load(&self) -> Result<Vec<Message>, SessionError> {
        // 物化前 = 空日志（没有文件就没有内容）。
        match self.inner.lock().expect("会话锁中毒").as_ref() {
            Some(store) => store.load(),
            None => Ok(Vec::new()),
        }
    }

    fn truncate(&self, len: usize) -> Result<(), SessionError> {
        match self.inner.lock().expect("会话锁中毒").as_ref() {
            Some(store) => store.truncate(len),
            // 物化前没有日志，截尾是空操作（幂等，与契约一致）。
            None => Ok(()),
        }
    }
}

/// 一个可选会话（`/session` 切换用）。
///
/// `name` 是唯一标识（文件 stem），`label` 给人看（自定义标题优先，否则回落
/// `name`），`preview` 是该会话首条用户消息的摘要——列表里只显示时间戳的话
/// 无法分辨，预览让用户认得出会话。
///
/// `modified_ms` / `turns` 是给 UI 的元信息（Codex 的 resume picker 会展示
/// 时间与轮数），不参与标识与查找。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub name: String,
    pub label: String,
    pub preview: String,
    /// 最近修改时间（Unix 毫秒；0 = 未知）。列表按它降序。
    pub modified_ms: u64,
    /// 用户轮数（该会话里 `User` 消息条数）。
    pub turns: usize,
}

impl SessionEntry {
    /// 该会话是否有内容（决定 UI 是否显示预览）。
    pub fn is_empty(&self) -> bool {
        self.preview.is_empty()
    }
}

/// 会话目录（应用层）：列出 / 打开 / 新建 / 重命名 / 删除会话。
///
/// 与 `models::ModelCatalog` 对称——把"当前有哪些会话可选、怎么管理"收敛到接口
/// 后面。当前实现是本地目录扫描（[`FileSessionCatalog`]），将来若要换成远端 /
/// 数据库，指令与选择器 UI 都不用改。
///
/// **TUI 与 desktop 共用同一个实现**：两边的会话列表 / 切换 / 重命名 / 删除都
/// 落在这一层，因此天然共享同一份 `<root>/.shirley/sessions` 数据。
pub trait SessionCatalog: Send + Sync {
    /// 列出全部会话，**按最近修改时间降序**（最新的在前）。
    fn list(&self) -> Result<Vec<SessionEntry>, SessionError>;

    /// 打开指定会话的存储（不存在则创建空文件）。
    fn open(&self, name: &str) -> Result<Arc<dyn SessionStore>, SessionError>;

    /// 新建一个空会话，返回它的条目与存储。
    fn create(&self) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError>;

    /// 惰性新建会话：**此刻不落盘**，返回的存储首次 `append` 时才创建文件。
    ///
    /// 用于「启动默认空会话」——没发起聊天就不该在会话列表里留一个空文件。
    /// 返回的 [`SessionEntry`] 此时 `preview` / `turns` 皆空、`name` 为占位
    /// （物化前的临时名，UI 不应据此查找会话）。
    ///
    /// 默认实现退化为 [`SessionCatalog::create`]（立即落盘）——不支持惰性的
    /// 后端无需改动，只是拿不到"延迟创建"的好处。
    fn create_lazy(
        &self,
        _title: Option<&str>,
    ) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        self.create()
    }

    /// 新建会话并可带标题（`None` = 匿名，UI 回落到首条用户消息 / 名字）。
    ///
    /// 默认实现忽略标题、退化为 [`SessionCatalog::create`]——不支持标题的后端
    /// 无需改动。
    fn create_named(
        &self,
        _title: Option<&str>,
    ) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        self.create()
    }

    /// 重命名会话：**只改标题，不改会话标识 / 内容**（Codex 的 `/rename` 语义，
    /// 不动 transcript）。
    ///
    /// 默认实现报错——不支持元数据的后端（如 [`EmptySessionCatalog`]）无需实现。
    fn rename(&self, _name: &str, _title: &str) -> Result<(), SessionError> {
        Err(SessionError::Backend("该会话目录不支持重命名".into()))
    }

    /// 删除会话（连同其日志与标题）。默认实现报错。
    fn delete(&self, _name: &str) -> Result<(), SessionError> {
        Err(SessionError::Backend("该会话目录不支持删除".into()))
    }
}

/// 基于本地目录的会话目录：`<root>/.shirley/sessions/<name>.jsonl`。
///
/// 每份会话就是一份独立的 JSONL 日志（沿用 [`JsonlSessionStore`]），
/// 目录扫描即"列出会话"。名字默认取时间戳，天然有序且无需额外索引。
///
/// 标题另存为 sidecar `<name>.title`（纯文本）：日志只存 `Message`
/// （`docs/session.md` 一.决策 1），把标题塞进消息流会破坏这条不变量。
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

    /// 标题 sidecar 路径：`<name>.title`（纯文本）。
    fn title_path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.title"))
    }

    /// 读取会话标题（未命名返回 `None`）。
    fn read_title(&self, name: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.title_path_for(name)).ok()?;
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }

    /// 写入会话标题：空串视为"清空标题"（删除 sidecar），非空则落盘。
    fn write_title(&self, name: &str, title: &str) -> Result<(), SessionError> {
        std::fs::create_dir_all(&self.dir)?;
        let title = title.trim();
        if title.is_empty() {
            let _ = std::fs::remove_file(self.title_path_for(name));
        } else {
            std::fs::write(self.title_path_for(name), title)?;
        }
        Ok(())
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
        let mut entries: Vec<(u64, SessionEntry)> = Vec::new();
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
            let modified_ms = item
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let (preview, turns) = summary_of(&path);
            entries.push((
                modified_ms,
                SessionEntry {
                    name: name.to_owned(),
                    label: self.read_title(name).unwrap_or_else(|| name.to_owned()),
                    preview,
                    modified_ms,
                    turns,
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
        self.create_named(None)
    }

    fn create_lazy(
        &self,
        title: Option<&str>,
    ) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        // 名字此刻就定（供 UI 显示），文件留到首次 append 才建。
        let catalog = Arc::new(FileSessionCatalog {
            dir: self.dir.clone(),
        });
        let name = self.fresh_name();
        let store = Arc::new(LazySessionStore::new(
            catalog,
            name.clone(),
            title.map(str::to_owned),
        ));
        let entry = SessionEntry {
            label: title.unwrap_or(name.as_str()).to_owned(),
            name,
            preview: String::new(),
            modified_ms: 0,
            turns: 0,
        };
        Ok((entry, store))
    }

    fn create_named(
        &self,
        title: Option<&str>,
    ) -> Result<(SessionEntry, Arc<dyn SessionStore>), SessionError> {
        let name = self.fresh_name();
        let store = self.open_named(&name)?;
        if let Some(title) = title {
            self.write_title(&name, title)?;
        }
        let entry = SessionEntry {
            label: self.read_title(&name).unwrap_or_else(|| name.clone()),
            name,
            preview: String::new(),
            modified_ms: 0,
            turns: 0,
        };
        Ok((entry, store))
    }

    fn rename(&self, name: &str, title: &str) -> Result<(), SessionError> {
        if !self.path_for(name).exists() {
            return Err(SessionError::Backend(format!("会话不存在：{name}")));
        }
        self.write_title(name, title)
    }

    fn delete(&self, name: &str) -> Result<(), SessionError> {
        let log = self.path_for(name);
        if !log.exists() {
            return Err(SessionError::Backend(format!("会话不存在：{name}")));
        }
        std::fs::remove_file(&log)?;
        // 标题 sidecar 附属于日志：日志没了它也就没意义，一并删除（best-effort）。
        let _ = std::fs::remove_file(self.title_path_for(name));
        Ok(())
    }
}

/// 读取会话的预览（首条用户消息，截断 40 字符）与用户轮数。读不到返回 `("", 0)`。
fn summary_of(path: &Path) -> (String, usize) {
    let Ok(file) = File::open(path) else {
        return (String::new(), 0);
    };
    let reader = BufReader::new(file);
    let mut preview = String::new();
    let mut turns = 0usize;
    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(Message::User { content }) = serde_json::from_str::<Message>(&line) {
            turns += 1;
            if preview.is_empty() {
                let one_line = content.replace('\n', " ");
                preview = truncate_chars(&one_line, 40);
            }
        }
    }
    (preview, turns)
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

/// 取会话里的**首条用户消息**正文（供自动标题当输入）。
///
/// 遍历顺序即消息顺序；`ContextSummary` / assistant / tool 消息跳过。没有用户
/// 消息时返回 `None`。正文按字符截断（[`AUTO_TITLE_INPUT_CHARS`]），避免把整段
/// 长输入喂给标题模型。
pub fn first_user_message(messages: &[Message]) -> Option<String> {
    messages.iter().find_map(|message| match message {
        Message::User { content } => Some(truncate_chars(content, AUTO_TITLE_INPUT_CHARS)),
        _ => None,
    })
}

/// 自动标题的一次性补全提示词：让模型把首条用户消息概括成短标题。
///
/// 只输出标题本身——清洗交给 [`sanitize_title`]（模型偶尔会带引号 / 前缀）。
pub fn title_prompt(first_user_message: &str) -> String {
    format!(
        "请阅读用户的第一条消息，用不超过 20 个字符概括这个会话的主题，作为会话标题。\
只输出标题本身：不要引号、不要标点、不要解释、不要换行。\n\n用户的第一条消息：\n{first_user_message}"
    )
}

/// 清洗模型返回的标题：取首个非空行、去掉包裹的引号 / 反引号与首尾空白、
/// 折叠内部空白、截断到 [`AUTO_TITLE_MAX_CHARS`]。清洗后为空返回 `None`。
pub fn sanitize_title(raw: &str) -> Option<String> {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let trimmed = line
        .trim_matches(|ch| matches!(ch, '"' | '\'' | '`' | '“' | '”' | '‘' | '’'))
        .trim();
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(truncate_chars(&collapsed, AUTO_TITLE_MAX_CHARS))
}

/// 自动标题的输入截断（字符数）。
const AUTO_TITLE_INPUT_CHARS: usize = 500;
/// 自动标题的输出截断（字符数）——与 `summary_of` 的预览长度同量级。
const AUTO_TITLE_MAX_CHARS: usize = 40;

/// 空会话目录：`App::new` 的默认值，供不关心会话切换的测试与默认构造使用。
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

    #[test]
    fn catalog_create_named_sets_title_and_counts_turns() {
        let dir = tmp_dir("named");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_named(Some("迁移任务")).unwrap();
        assert_eq!(entry.label, "迁移任务");
        store.append(&user("第一轮")).unwrap();
        store.append(&Message::Assistant {
            content: Some("回复".into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
        })
        .unwrap();
        store.append(&user("第二轮")).unwrap();
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        // 标题持久化在 sidecar，name 仍是时间戳（不改标识）。
        assert_eq!(listed[0].label, "迁移任务");
        assert_eq!(listed[0].name, entry.name);
        assert_eq!(listed[0].preview, "第一轮");
        assert_eq!(listed[0].turns, 2);
        assert!(listed[0].modified_ms > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_rename_changes_label_not_identity_or_content() {
        let dir = tmp_dir("rename");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create().unwrap();
        store.append(&user("内容")).unwrap();
        catalog.rename(&entry.name, "新名字").unwrap();
        let listed = catalog.list().unwrap();
        assert_eq!(listed[0].label, "新名字");
        // 标识与内容不变（rename 不动 transcript）。
        assert_eq!(listed[0].name, entry.name);
        assert_eq!(listed[0].preview, "内容");
        // 重开仍读到同样的日志。
        assert_eq!(catalog.open(&entry.name).unwrap().load().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_delete_removes_log_and_title() {
        let dir = tmp_dir("delete");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_named(Some("待删")).unwrap();
        store.append(&user("x")).unwrap();
        assert_eq!(catalog.list().unwrap().len(), 1);
        catalog.delete(&entry.name).unwrap();
        assert!(catalog.list().unwrap().is_empty());
        // 再删报错（已不存在）。
        assert!(catalog.delete(&entry.name).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_lazy_does_not_touch_disk_until_first_append() {
        let dir = tmp_dir("lazy");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_lazy(None).unwrap();
        // 构造后：名字已定，但目录里没有任何文件（不落盘）。
        assert!(!entry.name.is_empty());
        assert!(catalog.list().unwrap().is_empty());
        assert!(!catalog.path_for(&entry.name).exists());
        // 物化前 load 视为空日志。
        assert!(store.load().unwrap().is_empty());

        // 首次 append 才真正建文件。
        store.append(&user("第一条")).unwrap();
        assert!(catalog.path_for(&entry.name).exists());
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, entry.name);
        assert_eq!(listed[0].preview, "第一条");
        assert_eq!(listed[0].turns, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_lazy_with_title_writes_sidecar_on_materialize() {
        let dir = tmp_dir("lazy_title");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_lazy(Some("迁移任务")).unwrap();
        assert_eq!(entry.label, "迁移任务");
        // 物化前标题 sidecar 也不存在。
        assert!(!catalog.title_path_for(&entry.name).exists());
        store.append(&user("干活")).unwrap();
        let listed = catalog.list().unwrap();
        assert_eq!(listed[0].label, "迁移任务");
        assert_eq!(listed[0].preview, "干活");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_lazy_append_truncate_round_trips_after_materialize() {
        let dir = tmp_dir("lazy_rt");
        let catalog = FileSessionCatalog::new(&dir);
        let (entry, store) = catalog.create_lazy(None).unwrap();
        for c in ["a", "b", "c"] {
            store.append(&user(c)).unwrap();
        }
        // 物化后行为与普通会话一致：截尾 + 继续追加。
        store.truncate(2).unwrap();
        store.append(&user("d")).unwrap();
        let reopened = catalog.open(&entry.name).unwrap();
        assert_eq!(reopened.load().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_catalog_rejects_rename_and_delete() {
        let catalog = EmptySessionCatalog;
        assert!(catalog.rename("a", "b").is_err());
        assert!(catalog.delete("a").is_err());
    }

    #[test]
    fn first_user_message_skips_non_user_and_truncates() {
        let messages = vec![
            Message::Assistant {
                content: Some("hi".into()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            Message::User {
                content: "修复登录页的样式问题".into(),
            },
            Message::User {
                content: "第二条".into(),
            },
        ];
        assert_eq!(
            first_user_message(&messages).as_deref(),
            Some("修复登录页的样式问题")
        );
        // 无用户消息 → None。
        assert!(first_user_message(&[Message::Assistant {
            content: Some("x".into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
        }])
        .is_none());
        // 超长输入按字符截断并带省略号。
        let long = "字".repeat(AUTO_TITLE_INPUT_CHARS + 50);
        let got = first_user_message(&[Message::User { content: long }]).unwrap();
        assert!(got.ends_with('…'));
        assert_eq!(got.chars().count(), AUTO_TITLE_INPUT_CHARS + 1);
    }

    #[test]
    fn sanitize_title_strips_quotes_and_whitespace() {
        assert_eq!(sanitize_title("  “修复登录页样式”  ").as_deref(), Some("修复登录页样式"));
        assert_eq!(sanitize_title("`fix login`").as_deref(), Some("fix login"));
        // 取首个非空行。
        assert_eq!(sanitize_title("\n\n标题一行\n第二行").as_deref(), Some("标题一行"));
        // 折叠内部空白。
        assert_eq!(sanitize_title("a   b\tc").as_deref(), Some("a b c"));
        // 清洗后为空 → None。
        assert_eq!(sanitize_title("   \n  \n"), None);
        assert_eq!(sanitize_title("\"\""), None);
        // 超长截断。
        let long = "字".repeat(AUTO_TITLE_MAX_CHARS + 10);
        let got = sanitize_title(&long).unwrap();
        assert!(got.ends_with('…'));
    }
}
