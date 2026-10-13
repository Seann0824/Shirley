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
use crate::todo::TaskView;

/// 前端消费的事件 DTO。`#[serde(tag = "type", rename_all = "snake_case")]`
/// 与 `web/src/types/wire.ts` 的判别联合一一对应。
///
/// `session` 是事件的来源会话名（多会话下前端据此把事件路由到对应会话的视图，
/// 避免后台会话的流式增量串到前台）。`None` = 未打标（旧前端可忽略该字段）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventWire {
    ContentDelta {
        project: Option<String>,
        session: Option<String>,
        text: String,
    },
    MessageAdded {
        project: Option<String>,
        session: Option<String>,
        role: &'static str,
    },
    Compact {
        project: Option<String>,
        session: Option<String>,
        content: String,
    },
    ToolStarted {
        project: Option<String>,
        session: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolFinished {
        project: Option<String>,
        session: Option<String>,
        call_id: String,
        name: String,
        ok: bool,
        output: String,
        elapsed_ms: u64,
    },
    Usage {
        project: Option<String>,
        session: Option<String>,
        usage: UsageWire,
    },
    ContextUsage {
        project: Option<String>,
        session: Option<String>,
        used_tokens: u64,
        limit_tokens: u64,
    },
    CompressionStarted {
        project: Option<String>,
        session: Option<String>,
    },
    CompressionFinished {
        project: Option<String>,
        session: Option<String>,
    },
    /// 任务账本（todo）变更后的结构化快照。`todo: None` = 账本已清空。
    ///
    /// 账本只在模型调用 `todo` 工具时变化，故后端在 `todo` 工具完成时发一条；
    /// 前端据此渲染/更新任务卡，而不是解析注入给模型的 XML。
    TodoUpdated {
        project: Option<String>,
        session: Option<String>,
        todo: Option<TaskViewWire>,
    },
    Finished {
        project: Option<String>,
        session: Option<String>,
        stop_reason: &'static str,
    },
    Error {
        project: Option<String>,
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

/// 任务账本的结构化线格式（与 `web/src/types/wire.ts` 的 `TaskViewWire` 对齐）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskViewWire {
    pub goal: Option<String>,
    pub steps: Vec<TaskStepWire>,
    pub completed: usize,
    pub total: usize,
    pub current: Option<String>,
}

/// 任务账本里的一步。
#[derive(Debug, Clone, Serialize)]
pub struct TaskStepWire {
    pub text: String,
    /// `pending` / `in_progress` / `completed`（由 `TodoStatus` 序列化）。
    pub status: crate::todo::TodoStatus,
}

impl From<TaskView> for TaskViewWire {
    fn from(view: TaskView) -> Self {
        Self {
            goal: view.goal,
            steps: view
                .steps
                .into_iter()
                .map(|step| TaskStepWire {
                    text: step.text,
                    status: step.status,
                })
                .collect(),
            completed: view.completed,
            total: view.total,
            current: view.current,
        }
    }
}

impl AgentEventWire {
    /// 把内部事件翻译成线格式。`MessageAdded` 只透出角色（前端不消费正文，
    /// 正文已由增量事件推送）。`project` / `session` 先留空，由 [`Self::with_route`] 打标。
    ///
    /// **思考（`ReasoningDelta`）返回 `None`**：思考过程程序内部消化，不展示给用户，
    /// 故不进线格式、不发给前端。返回 `Option` 让调用方直接丢弃。
    pub fn from_event(event: AgentEvent) -> Option<Self> {
        let wire = match event {
            // 思考不进 UI：直接吞掉（SDK 仍会产出该事件，供内部观测）。
            AgentEvent::ReasoningDelta(_) => return None,
            AgentEvent::ContentDelta(text) => Self::ContentDelta {
                project: None,
                session: None,
                text,
            },
            AgentEvent::MessageAdded(shirley_agent_sdk::Message::ContextSummary { content }) => {
                Self::Compact {
                    project: None,
                    session: None,
                    content,
                }
            }
            AgentEvent::MessageAdded(message) => Self::MessageAdded {
                project: None,
                session: None,
                role: message_role(&message),
            },
            AgentEvent::ToolStarted {
                call_id,
                name,
                arguments,
            } => Self::ToolStarted {
                project: None,
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
                project: None,
                session: None,
                call_id,
                name,
                ok,
                output,
                elapsed_ms,
            },
            AgentEvent::Usage(usage) => Self::Usage {
                project: None,
                session: None,
                usage: usage.into(),
            },
            AgentEvent::ContextUsage {
                used_tokens,
                limit_tokens,
            } => Self::ContextUsage {
                project: None,
                session: None,
                used_tokens,
                limit_tokens,
            },
            AgentEvent::CompressionStarted => Self::CompressionStarted {
                project: None,
                session: None,
            },
            AgentEvent::CompressionFinished => Self::CompressionFinished {
                project: None,
                session: None,
            },
            AgentEvent::Finished(result) => Self::Finished {
                project: None,
                session: None,
                stop_reason: match result.stop_reason {
                    shirley_agent_sdk::StopReason::Completed => "completed",
                    shirley_agent_sdk::StopReason::MaxStepsReached => "max_steps_reached",
                    shirley_agent_sdk::StopReason::Cancelled => "cancelled",
                },
            },
        };
        Some(wire)
    }

    /// 给事件打上来源项目 + 会话（多项目多会话下前端据此路由）。
    ///
    /// 项目 id 全局唯一（规范路径），会话名只在项目内唯一——两者一起才是
    /// 前端的路由键（与后端 pump 的 `project::session` 键对应）。
    pub fn with_route(mut self, project: &str, session: &str) -> Self {
        let (project_slot, session_slot) = match &mut self {
            Self::ContentDelta {
                project, session, ..
            }
            | Self::MessageAdded {
                project, session, ..
            }
            | Self::Compact {
                project, session, ..
            }
            | Self::ToolStarted {
                project, session, ..
            }
            | Self::ToolFinished {
                project, session, ..
            }
            | Self::Usage {
                project, session, ..
            }
            | Self::ContextUsage {
                project, session, ..
            }
            | Self::CompressionStarted { project, session }
            | Self::CompressionFinished { project, session }
            | Self::TodoUpdated {
                project, session, ..
            }
            | Self::Finished {
                project, session, ..
            }
            | Self::Error {
                project, session, ..
            } => (project, session),
        };
        *project_slot = Some(project.to_owned());
        *session_slot = Some(session.to_owned());
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
    Message { role: String, content: String },
    Compact { content: String },
    Tools { calls: Vec<ToolCallWireDto> },
}

/// 快照里单次工具调用的 DTO。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallWireDto {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl From<CoreItemWire> for ItemWire {
    fn from(item: CoreItemWire) -> Self {
        match item {
            CoreItemWire::Compact { content } => ItemWire::Compact { content },
            CoreItemWire::Message { role, content } => ItemWire::Message {
                role: role_wire(role).to_owned(),
                content,
            },
            CoreItemWire::Tools { calls } => ItemWire::Tools {
                calls: calls
                    .into_iter()
                    .map(
                        |ToolCallWire {
                             id,
                             name,
                             arguments,
                         }| ToolCallWireDto {
                            id,
                            name,
                            arguments,
                        },
                    )
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
    /// 该会话当前的任务账本快照（`None` = 空账本 / 无账本）。
    pub todo: Option<TaskViewWire>,
}

impl SessionSnapshotWire {
    /// 由核心层条目构造（会话名 + 条目序列 + 任务账本快照）。
    pub fn new(
        session: String,
        items: Vec<CoreItemWire>,
        running: bool,
        todo: Option<TaskView>,
    ) -> Self {
        Self {
            session,
            items: items.into_iter().map(ItemWire::from).collect(),
            running,
            todo: todo.map(TaskViewWire::from),
        }
    }
}
