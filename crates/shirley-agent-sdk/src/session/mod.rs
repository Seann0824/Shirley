//! 会话持久化（`docs/session.md`）。
//!
//! 定位：**SDK 定契约，应用层填实现**。不管 SQLite / JSONL / Postgres，
//! SDK 只依赖 [`SessionStore`] 这一个抽象；存储技术细节完全在应用层。
//!
//! 三条核心不变量（`docs/session.md` 五）：
//!
//! 1. **持久化的是原始 Message 全量日志**——不是 chunk、不是 BM25 索引、
//!    也不是 system 提示词。召回库（`recall`）由日志派生，不单独落盘。
//! 2. **日志只追加**，唯一例外是 rewind 截尾（且只截尾，不留中间空洞）。
//! 3. **`ContextSummary` 是日志里的一等消息**——压缩只追加一条摘要，不重写日志。
//!
//! system 提示词**不入日志**：它可能是函数形式、随工作目录 / `Agent.md` 变化，
//! 恢复时由 `Agent` 现生成（与压缩重建同一套规则）。

use crate::error::{ErrorKind, SdkError};
use crate::message::Message;
use std::sync::Mutex;

/// 会话日志的存储抽象。
///
/// 三个方法对应三种操作，**恰好是架构里的全部写入语义**：
/// 追加（正常轮次 + 压缩）、读回（恢复）、截尾（rewind）。
/// 故意没有 `delete` / `update` / `search`——用不到。
///
/// **签名是同步的**：`Agent::new` 是同步构造，恢复发生在构造时，
/// 同步接口让 `Agent::new` 不必变异步。存储后端若是异步驱动，
/// 由应用层内部消化（同步驱动如 `rusqlite` 最省事）。契约同步，实现自由。
pub trait SessionStore: Send + Sync {
    /// 追加一条消息到日志尾部。压缩产生的 `ContextSummary` 也走这里。
    fn append(&self, message: &Message) -> Result<(), SessionError>;

    /// 读取全量日志（只读、顺序）。
    fn load(&self) -> Result<Vec<Message>, SessionError>;

    /// 截断到前 `len` 条（rewind 用；只截尾，不产生中间空洞）。
    ///
    /// `len` 不小于当前长度时不做任何事（幂等，避免越界）——与
    /// `Agent::rewind` 的幂等语义一致。
    fn truncate(&self, len: usize) -> Result<(), SessionError>;
}

/// 会话存储失败。
///
/// 展示格式统一为 `[前缀]: 详情`，与 `AdapterError` / `ToolError` /
/// `SandboxError` / `WorkspaceError` 一致；前缀留在变体旁，不从
/// [`ErrorKind`] 推导。
///
/// 当前 `append` 失败一律**中断本轮**（`docs/session.md` 二.2）：持久化
/// 失败还继续跑，等于假装有存档，比直接报错更危险。因此默认 `Internal`
/// （存储 I/O 错误不可重试）；将来若区分出瞬时故障，再细化 kind。
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// 底层存储 I/O 失败（写盘、读盘、序列化等）。
    #[error("[会话存储失败]: {0}")]
    Io(#[from] std::io::Error),

    /// 存储后端本身报告的错误（例如 SQL 失败），带自由文本详情。
    #[error("[会话存储错误]: {0}")]
    Backend(String),
}

impl SdkError for SessionError {
    fn kind(&self) -> ErrorKind {
        // 存储失败当前不可重试。细化分类等出现真实可重试场景再说。
        ErrorKind::Internal
    }
}

/// 内存实现：给测试用，也给"走同一路径但不落盘"的兜底。
///
/// 不传 `session` 时 `Agent` 行为与现状完全一致；传 `InMemoryStore`
/// 则跑通完整日志路径（append / load / truncate），但进程结束即失。
pub struct InMemoryStore {
    log: Mutex<Vec<Message>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            log: Mutex::new(Vec::new()),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore for InMemoryStore {
    fn append(&self, message: &Message) -> Result<(), SessionError> {
        self.log
            .lock()
            .expect("session 锁中毒")
            .push(message.clone());
        Ok(())
    }

    fn load(&self) -> Result<Vec<Message>, SessionError> {
        Ok(self.log.lock().expect("session 锁中毒").clone())
    }

    fn truncate(&self, len: usize) -> Result<(), SessionError> {
        let mut log = self.log.lock().expect("session 锁中毒");
        if len < log.len() {
            log.truncate(len);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: &str) -> Message {
        Message::User {
            content: content.to_owned(),
        }
    }

    #[test]
    fn append_then_load_preserves_order() {
        let store = InMemoryStore::new();
        store.append(&user("a")).unwrap();
        store.append(&user("b")).unwrap();
        store.append(&user("c")).unwrap();

        let log = store.load().unwrap();
        assert_eq!(log.len(), 3);
        match (&log[0], &log[2]) {
            (Message::User { content: first }, Message::User { content: last }) => {
                assert_eq!(first, "a");
                assert_eq!(last, "c");
            }
            _ => panic!("应为 User 消息"),
        }
    }

    #[test]
    fn truncate_drops_tail() {
        let store = InMemoryStore::new();
        for c in ["a", "b", "c", "d"] {
            store.append(&user(c)).unwrap();
        }
        store.truncate(2).unwrap();
        assert_eq!(store.load().unwrap().len(), 2);
    }

    #[test]
    fn truncate_is_idempotent_when_len_not_smaller() {
        let store = InMemoryStore::new();
        store.append(&user("a")).unwrap();
        // len >= 当前长度：不动，也不越界。
        store.truncate(5).unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
    }

    #[test]
    fn context_summary_round_trips() {
        // ContextSummary 是日志里的一等消息，必须能原样存取。
        let store = InMemoryStore::new();
        store
            .append(&Message::ContextSummary {
                content: "<current_goal>x</current_goal>".to_owned(),
            })
            .unwrap();
        match &store.load().unwrap()[0] {
            Message::ContextSummary { content } => assert!(content.contains("current_goal")),
            _ => panic!("应为 ContextSummary"),
        }
    }

    #[test]
    fn error_kind_is_internal() {
        let error = SessionError::Backend("boom".into());
        assert_eq!(error.kind(), ErrorKind::Internal);
        assert!(!error.is_retryable());
    }
}
