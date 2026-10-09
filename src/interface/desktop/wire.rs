//! `AgentEvent` → 线格式 DTO 映射。
//!
//! 为什么不让 `AgentEvent` 直接派生 `Serialize`：SDK 的对外契约要保持小，
//! 协议/界面差异收敛在边界（`docs/README.md` 原则一、`docs/desktop-interface.md`
//! 第六节）。这里把内部事件翻译成前端能消费的 JSON，SDK 不为界面新增对外类型。
//!
//! 唯一对 SDK 的改动是为支撑工具卡给 `ToolStarted` / `ToolFinished` **追加**字段
//! （`arguments` / `ok` / `output` / `elapsed_ms`）——追加不破坏既有消费者。
//!
//! 线格式契约见 `web/src/types/wire.ts`，两边必须同步。

#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

use serde::Serialize;
use shirley_agent_sdk::AgentEvent;
use shirley_agent_sdk::Usage;

use crate::interface::session::{ItemWire as CoreItemWire, ToolCallWire};

/// 前端消费的事件 DTO。`#[serde(tag = "type", rename_all = "snake_case")]`
/// 与 `web/src/types/wire.ts` 的判别联合一一对应。
///
/// `session` 是事件的来源会话名（多会话下前端据此把事件路由到对应会话的视图，
/// 避免后台会话的流式增量串到前台）。`None` = 未打标（旧前端可忽略该字段）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventWire {
    ContentDelta {
        session: Option<String>,
        text: String,
    },
    ReasoningDelta {
        session: Option<String>,
        text: String,
    },
    MessageAdded {
        session: Option<String>,
        role: &'static str,
    },
    ToolStarted {
        session: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolFinished {
        session: Option<String>,
        call_id: String,
        name: String,
        ok: bool,
        output: String,
        elapsed_ms: u64,
    },
    Usage {
        session: Option<String>,
        usage: UsageWire,
    },
    ContextUsage {
        session: Option<String>,
        used_tokens: u64,
        limit_tokens: u64,
    },
    CompressionStarted {
        session: Option<String>,
    },
    CompressionFinished {
        session: Option<String>,
    },
    Finished {
        session: Option<String>,
        stop_reason: &'static str,
    },
    Error {
        session: Option<String>,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageWire {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` = 供应商未上报，区别于上报 0。
    pub cached_input_tokens: Option<u64>,
    pub cache_reported_input_tokens: Option<u64>,
}

impl From<Usage> for UsageWire {
    fn from(usage: Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_reported_input_tokens: usage.cache_reported_input_tokens,
        }
    }
}

impl AgentEventWire {
    /// 把内部事件翻译成线格式。`MessageAdded` 只透出角色（前端不消费正文，
    /// 正文已由增量事件推送）。`session` 先留空，由 [`Self::with_session`] 打标。
    pub fn from_event(event: AgentEvent) -> Self {
        match event {
            AgentEvent::ContentDelta(text) => Self::ContentDelta {
                session: None,
                text,
            },
            AgentEvent::ReasoningDelta(text) => Self::ReasoningDelta {
                session: None,
                text,
            },
            AgentEvent::MessageAdded(message) => Self::MessageAdded {
                session: None,
                role: message_role(&message),
            },
            AgentEvent::ToolStarted {
                call_id,
                name,
                arguments,
            } => Self::ToolStarted {
                session: None,
                call_id,
                name,
                arguments,
            },
            AgentEvent::ToolFinished {
                call_id,
                name,
                ok,
                output,
                elapsed_ms,
            } => Self::ToolFinished {
                session: None,
                call_id,
                name,
                ok,
                output,
                elapsed_ms,
            },
            AgentEvent::Usage(usage) => Self::Usage {
                session: None,
                usage: usage.into(),
            },
            AgentEvent::ContextUsage {
                used_tokens,
                limit_tokens,
            } => Self::ContextUsage {
                session: None,
                used_tokens,
                limit_tokens,
            },
            AgentEvent::CompressionStarted => Self::CompressionStarted { session: None },
            AgentEvent::CompressionFinished => Self::CompressionFinished { session: None },
            AgentEvent::Finished(result) => Self::Finished {
                session: None,
                stop_reason: match result.stop_reason {
                    shirley_agent_sdk::StopReason::Completed => "completed",
                    shirley_agent_sdk::StopReason::MaxStepsReached => "max_steps_reached",
                    shirley_agent_sdk::StopReason::Cancelled => "cancelled",
                },
            },
        }
    }

    /// 给事件打上来源会话名（多会话下前端据此路由）。
    pub fn with_session(mut self, session: &str) -> Self {
        let slot = match &mut self {
            Self::ContentDelta { session, .. }
            | Self::ReasoningDelta { session, .. }
            | Self::MessageAdded { session, .. }
            | Self::ToolStarted { session, .. }
            | Self::ToolFinished { session, .. }
            | Self::Usage { session, .. }
            | Self::ContextUsage { session, .. }
            | Self::CompressionStarted { session }
            | Self::CompressionFinished { session }
            | Self::Finished { session, .. }
            | Self::Error { session, .. } => session,
        };
        *slot = Some(session.to_owned());
        self
    }
}

fn message_role(message: &shirley_agent_sdk::Message) -> &'static str {
    use shirley_agent_sdk::Message;
    match message {
        Message::System { .. } => "system",
        Message::User { .. } => "user",
        Message::Assistant { .. } => "assistant",
        Message::Tool { .. } => "tool",
        Message::ContextSummary { .. } => "context_summary",
    }
}

/// 会话视图快照里的一条 UI 条目（与 `web/src/types/wire.ts` 的 `ItemWire` 对齐）。
///
/// 由核心层 [`CoreItemWire`]（`interface::session::ItemWire`）1:1 映射而来：
/// UI 订阅一个已在运行 / 已积累历史的会话时，先拿快照重建视图，再叠加之后的事件。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemWire {
    Message {
        role: String,
        content: String,
        thinking: bool,
    },
    Tools {
        calls: Vec<ToolCallWireDto>,
    },
}

/// 快照里单次工具调用的 DTO。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallWireDto {
    pub name: String,
    pub arguments: String,
}

impl From<CoreItemWire> for ItemWire {
    fn from(item: CoreItemWire) -> Self {
        match item {
            CoreItemWire::Message {
                role,
                content,
                thinking,
            } => ItemWire::Message {
                role: role_wire(role).to_owned(),
                content,
                thinking,
            },
            CoreItemWire::Tools { calls } => ItemWire::Tools {
                calls: calls
                    .into_iter()
                    .map(|ToolCallWire { name, arguments }| ToolCallWireDto { name, arguments })
                    .collect(),
            },
        }
    }
}

/// UI 角色 → 线格式字符串（与 `web/src/types/wire.ts` 的 `role` 对齐）。
fn role_wire(role: crate::interface::app::Role) -> &'static str {
    use crate::interface::app::Role;
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Summary => "summary",
        Role::System => "system",
        Role::Error => "error",
    }
}

/// 会话视图快照：一次订阅时下发给 UI 的完整可重建条目列表。
#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshotWire {
    pub session: String,
    pub items: Vec<ItemWire>,
    /// 该会话当前是否在跑一轮（前端据此决定 busy 门）。
    pub running: bool,
}

impl SessionSnapshotWire {
    /// 由核心层条目构造（会话名 + 条目序列）。
    pub fn new(session: String, items: Vec<CoreItemWire>, running: bool) -> Self {
        Self {
            session,
            items: items.into_iter().map(ItemWire::from).collect(),
            running,
        }
    }
}
