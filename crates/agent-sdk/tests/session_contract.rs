//! 会话持久化的公开契约测试（`docs/session.md`）。
//!
//! 锁的是这几条对外承诺：
//!
//! 1. `messages` 为空且有 `session` 时，`Agent::new` 从日志恢复工作集；
//! 2. 恢复出的历史**不含 system**——system 由 `system_prompt` 现生成置顶；
//! 3. `rewind_last_user_turn` 同步截断日志（只截尾），且返回被丢弃的用户原文；
//! 4. 不挂 session 时行为不变（纯内存）。
//!
//! 只依赖 `agent_sdk` 的公开 API。

use agent_sdk::{Agent, InMemoryStore, Message, ModelConfig, ModelProtocol, SessionStore};
use std::sync::Arc;

fn model_config() -> ModelConfig {
    ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url("http://localhost")
        .model("test")
        .build()
}

fn user(text: &str) -> Message {
    Message::User {
        content: text.into(),
    }
}

fn assistant(text: &str) -> Message {
    Message::Assistant {
        content: Some(text.into()),
        reasoning_content: None,
        tool_calls: Vec::new(),
    }
}

/// messages 为空时，从 session 日志恢复（不含 system）。
#[test]
fn restores_messages_from_session_when_empty() {
    let store: Arc<dyn SessionStore> = Arc::new(InMemoryStore::new());
    store.append(&user("第一轮")).unwrap();
    store.append(&assistant("回复一")).unwrap();
    store.append(&user("第二轮")).unwrap();

    let agent = Agent::builder()
        .model_config(model_config())
        .session(store.clone())
        .build()
        .unwrap();

    // 空提示词 → 不置顶 system；恢复出的就是日志三条。
    let messages = agent.messages();
    assert_eq!(messages.len(), 3, "应从日志恢复三条: {messages:?}");
    assert!(matches!(messages[0], Message::User { .. }));
}

/// 恢复出的历史不含 system；system 由 system_prompt 现生成置顶。
#[test]
fn restore_regenerates_system_and_strips_it_from_log() {
    let store: Arc<dyn SessionStore> = Arc::new(InMemoryStore::new());
    // 故意往日志里塞一条 system（防御性剔除的路径）。
    store
        .append(&Message::System {
            content: "旧的 system".into(),
        })
        .unwrap();
    store.append(&user("你好")).unwrap();

    let agent = Agent::builder()
        .model_config(model_config())
        .system_prompt("当前 system")
        .session(store.clone())
        .build()
        .unwrap();

    let messages = agent.messages();
    assert_eq!(messages.len(), 2, "应为 [新 system, user]: {messages:?}");
    match &messages[0] {
        Message::System { content } => assert_eq!(content, "当前 system"),
        other => panic!("首条应为现生成的 system，实际 {other:?}"),
    }
    // 新生成的 system 不该被写回日志（system 不入日志，恢复时现生成）。
    let log = store.load().unwrap();
    assert!(
        !log.iter().any(|m| matches!(m, Message::System { content } if content == "当前 system")),
        "新 system 不应进入日志: {log:?}"
    );
    // 日志仍保持原样两条（防御性剔除只作用于内存工作集，不重写日志）。
    assert_eq!(log.len(), 2, "日志不应被恢复路径改写: {log:?}");
}

/// rewind_last_user_turn 返回被丢弃的用户原文，并同步截断日志。
#[test]
fn rewind_truncates_messages_and_session() {
    // 走恢复路径：日志里有完整一轮，Agent 从日志重建 messages。
    let store: Arc<dyn SessionStore> = Arc::new(InMemoryStore::new());
    store.append(&user("问题一")).unwrap();
    store.append(&assistant("回答一")).unwrap();
    store.append(&user("问题二")).unwrap();

    let mut agent = Agent::builder()
        .model_config(model_config())
        // 空提示词 → 不置顶 system，便于按日志长度断言。
        .session(store.clone())
        .build()
        .unwrap();
    assert_eq!(agent.messages().len(), 3, "应先从日志恢复三条");

    let reverted = agent.rewind_last_user_turn().unwrap();
    assert_eq!(reverted.as_deref(), Some("问题二"), "应返回最后一条用户原文");
    // 内存：问题二被丢弃，剩两条。
    assert_eq!(agent.messages().len(), 2, "回退后剩两条历史");
    // 日志：同步截到相同长度（此处无 system，长度相等）。
    assert_eq!(store.load().unwrap().len(), 2, "日志应同步截尾");
}

/// 没有用户消息时 rewind 返回 None，且不改动任何东西。
#[test]
fn rewind_without_user_message_is_noop() {
    let store: Arc<dyn SessionStore> = Arc::new(InMemoryStore::new());
    store.append(&assistant("只有助手消息")).unwrap();
    let mut agent = Agent::builder()
        .model_config(model_config())
        .session(store.clone())
        .build()
        .unwrap();

    assert!(agent.rewind_last_user_turn().unwrap().is_none());
    assert_eq!(store.load().unwrap().len(), 1, "日志不应被改动");
}

/// 不挂 session 时是纯内存，行为与现状一致。
#[test]
fn without_session_is_pure_memory() {
    let agent = Agent::builder()
        .model_config(model_config())
        .messages(vec![user("a")])
        .build()
        .unwrap();
    assert_eq!(agent.messages().len(), 1);
}

/// `switch_session` 换掉日志源，并按恢复规则重建工作集（含现生成 system 置顶）。
#[test]
fn switch_session_swaps_workingset_and_log_target() {
    let store_a: Arc<dyn SessionStore> = Arc::new(InMemoryStore::new());
    store_a.append(&user("会话A的用户")).unwrap();
    store_a.append(&assistant("会话A的回复")).unwrap();

    let store_b: Arc<dyn SessionStore> = Arc::new(InMemoryStore::new());
    store_b.append(&user("会话B的用户")).unwrap();

    // 从 A 恢复。
    let mut agent = Agent::builder()
        .model_config(model_config())
        .system_prompt("当前 system")
        .session(store_a.clone())
        .build()
        .unwrap();
    assert_eq!(agent.messages().len(), 3, "应为 [system, A用户, A回复]");

    // 切到 B：工作集被整体替换，system 现生成置顶。
    agent.switch_session(store_b.clone()).unwrap();
    let messages = agent.messages();
    assert_eq!(messages.len(), 2, "应为 [新 system, B用户]: {messages:?}");
    match &messages[0] {
        Message::System { content } => assert_eq!(content, "当前 system"),
        other => panic!("首条应为现生成的 system，实际 {other:?}"),
    }
    assert!(matches!(&messages[1], Message::User { content } if content == "会话B的用户"));

    // 后续 append 落到新日志（B），旧日志（A）不再被写入。
    agent.rewind_last_user_turn().unwrap();
    assert_eq!(store_b.load().unwrap().len(), 0, "截断应作用于新日志 B");
    assert_eq!(store_a.load().unwrap().len(), 2, "旧日志 A 不应被改动");
}

/// `switch_session` 到空日志：只剩现生成的 system（若有），旧会话不残留。
#[test]
fn switch_session_to_empty_clears_workingset() {
    let store_a: Arc<dyn SessionStore> = Arc::new(InMemoryStore::new());
    store_a.append(&user("旧内容")).unwrap();
    let empty: Arc<dyn SessionStore> = Arc::new(InMemoryStore::new());

    let mut agent = Agent::builder()
        .model_config(model_config())
        .session(store_a.clone())
        .build()
        .unwrap();
    assert_eq!(agent.messages().len(), 1);

    agent.switch_session(empty).unwrap();
    // 无 system_prompt → 空日志恢复出空工作集。
    assert!(agent.messages().is_empty(), "切到空日志后工作集应为空");
}
