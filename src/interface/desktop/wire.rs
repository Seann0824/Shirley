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

/// 前端消费的事件 DTO。`#[serde(tag = "type", rename_all = "snake_case")]`
/// 与 `web/src/types/wire.ts` 的判别联合一一对应。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventWire {
    ContentDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    MessageAdded {
        role: &'static str,
    },
    ToolStarted {
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolFinished {
        call_id: String,
        name: String,
        ok: bool,
        output: String,
        elapsed_ms: u64,
    },
    Usage {
        usage: UsageWire,
    },
    ContextUsage {
        used_tokens: u64,
        limit_tokens: u64,
    },
    CompressionStarted,
    CompressionFinished,
    Finished {
        stop_reason: &'static str,
    },
    Error {
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
    /// 正文已由增量事件推送）。
    pub fn from_event(event: AgentEvent) -> Self {
        match event {
            AgentEvent::ContentDelta(text) => Self::ContentDelta { text },
            AgentEvent::ReasoningDelta(text) => Self::ReasoningDelta { text },
            AgentEvent::MessageAdded(message) => Self::MessageAdded {
                role: message_role(&message),
            },
            AgentEvent::ToolStarted {
                call_id,
                name,
                arguments,
            } => Self::ToolStarted {
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
                call_id,
                name,
                ok,
                output,
                elapsed_ms,
            },
            AgentEvent::Usage(usage) => Self::Usage {
                usage: usage.into(),
            },
            AgentEvent::ContextUsage {
                used_tokens,
                limit_tokens,
            } => Self::ContextUsage {
                used_tokens,
                limit_tokens,
            },
            AgentEvent::CompressionStarted => Self::CompressionStarted,
            AgentEvent::CompressionFinished => Self::CompressionFinished,
            AgentEvent::Finished(result) => Self::Finished {
                stop_reason: match result.stop_reason {
                    shirley_agent_sdk::StopReason::Completed => "completed",
                    shirley_agent_sdk::StopReason::MaxStepsReached => "max_steps_reached",
                    shirley_agent_sdk::StopReason::Cancelled => "cancelled",
                },
            },
        }
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
