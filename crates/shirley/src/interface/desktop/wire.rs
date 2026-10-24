//! `AgentEvent` → wire-format DTO mapping.
//!
//! Why not let `AgentEvent` derive `Serialize` directly: the SDK's public contract
//! must stay small, and protocol/UI differences are contained at the boundary
//! (`docs/README.md` principle 1, `docs/desktop-interface.md` section 6). Here we
//! translate internal events into JSON the frontend can consume; the SDK gains no
//! new public types for the interface.
//!
//! The only SDK change is **appending** fields to `ToolStarted` / `ToolFinished` to
//! support tool cards (`arguments` / `ok` / `output` / `elapsed_ms`) — appending does
//! not break existing consumers.
//!
//! The wire-format contract lives in `web/src/types/wire.ts`; both sides must stay in sync.

#![cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        reason = "only used when the relevant interface feature is enabled"
    )
)]

use serde::Serialize;
use shirley_agent_sdk::AgentEvent;
use shirley_agent_sdk::ErrorKind;
use shirley_agent_sdk::Usage;

use crate::interface::session::{ItemWire as CoreItemWire, ToolCallWire};
use crate::status::{Activity, Health, Liveness, StateSnapshot};
use crate::todo::TaskView;

/// Stable `snake_case` wire name for an [`ErrorKind`]. The frontend switches on these
/// strings; they mirror the enum variant names and must stay in sync with `types/wire.ts`.
fn error_kind_str(kind: ErrorKind) -> String {
    match kind {
        ErrorKind::Transport => "transport",
        ErrorKind::RateLimited => "rate_limited",
        ErrorKind::ServerError => "server_error",
        ErrorKind::BadRequest => "bad_request",
        ErrorKind::Unsupported => "unsupported",
        ErrorKind::ToolFailure => "tool_failure",
        ErrorKind::Internal => "internal",
    }
    .to_string()
}

/// Event DTO consumed by the frontend. `#[serde(tag = "type", rename_all = "snake_case")]`
/// maps one-to-one to the discriminated union in `web/src/types/wire.ts`.
///
/// `session` is the event's source session name (with multiple sessions the frontend
/// routes the event to the right session's view, so a background session's streaming
/// deltas don't leak into the foreground). `None` = untagged (older frontends may
/// ignore this field).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventWire {
    Retrying {
        project: Option<String>,
        session: Option<String>,
        attempt: usize,
        reason: String,
    },
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
        /// Stable error class on failure (`None` on success), as a `snake_case` string the
        /// frontend switches on. This replaces sniffing the error text for classification.
        error_kind: Option<String>,
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
    /// Structured snapshot after the task ledger (todo) changes. `todo: None` = ledger cleared.
    ///
    /// The ledger only changes when the model calls the `todo` tool, so the backend
    /// emits one when the `todo` tool finishes; the frontend renders/updates the task
    /// card from this instead of parsing the XML injected into the model.
    TodoUpdated {
        project: Option<String>,
        session: Option<String>,
        todo: Option<TaskViewWire>,
    },
    /// A neutral, human-readable notice (not an error, not model content). The frontend shows
    /// it inline as muted text. Used for silent image degradation.
    Notice {
        project: Option<String>,
        session: Option<String>,
        text: String,
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
#[expect(
    clippy::struct_field_names,
    reason = "field names are the frontend wire contract; the `_tokens` postfix mirrors the SDK Usage type"
)]
pub struct UsageWire {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` = the provider did not report, distinct from reporting 0.
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

/// Content-free runtime-status payload for the **`status://event`** channel.
///
/// This is deliberately **not** an `AgentEventWire` variant (`docs/agent-status.md` §6.1): the render
/// channel carries content and only fires when an `AgentEvent` arrives, whereas the status stream is
/// content-free and ticks even when nothing else happens (that is the whole point of stall
/// detection). Keeping them separate is the A5 "content-free" guarantee.
///
/// `Instant` is not serializable, so the two time fields are exposed as *durations in seconds* —
/// derived at snapshot time, never carrying a wall-clock the UI has to reconcile.
#[derive(Debug, Clone, Serialize)]
pub struct StateSnapshotWire {
    pub project: Option<String>,
    pub session: Option<String>,
    /// `idle` / `running` / `stalled` / `unreachable`.
    pub liveness: String,
    /// `healthy` / `slow_tool` / `retrying` / `compacting` / `failed`.
    pub health: String,
    /// Human-facing detail for the health state (e.g. the slow tool's name, the retry reason).
    /// Never prompt / output / tool-argument content.
    pub health_detail: Option<String>,
    /// `model_call` / `tool_call` / `compacting` / `waiting_input` (`null` when idle).
    pub activity: Option<String>,
    /// +1 per completed model call / tool call within the turn.
    pub progress_seq: u64,
    /// Seconds since `progress_seq` last advanced ("last real progress").
    pub seconds_since_progress: Option<u64>,
    /// Seconds since the current turn started ("Worked for Ns").
    pub seconds_since_turn_start: Option<u64>,
    /// True when the source that produced this snapshot is gone (render muted).
    pub stale: bool,
}

impl StateSnapshotWire {
    pub fn from_snapshot(snapshot: &StateSnapshot) -> Self {
        let (health, health_detail) = match &snapshot.health {
            Health::Healthy => ("healthy".to_owned(), None),
            Health::SlowTool { name, elapsed_ms } => (
                "slow_tool".to_owned(),
                Some(format!("{name} · {elapsed_ms} ms")),
            ),
            // Only the attempt count is user-facing: the raw upstream error body
            // (HTTP status + JSON) is noise to a user, so it never reaches the wire.
            Health::Retrying { attempt, .. } => {
                ("retrying".to_owned(), Some(format!("attempt {attempt}")))
            }
            Health::Compacting => ("compacting".to_owned(), None),
            Health::Failed { message, .. } => ("failed".to_owned(), Some(message.clone())),
        };
        let liveness = match snapshot.liveness {
            Liveness::Idle => "idle",
            Liveness::Running => "running",
            Liveness::Stalled => "stalled",
            Liveness::Unreachable => "unreachable",
        };
        let activity = snapshot.activity.map(|activity| match activity {
            Activity::ModelCall => "model_call",
            Activity::ToolCall => "tool_call",
            Activity::Compacting => "compacting",
            Activity::WaitingInput => "waiting_input",
        });
        let now = std::time::Instant::now();
        Self {
            project: None,
            session: None,
            liveness: liveness.to_owned(),
            health,
            health_detail,
            activity: activity.map(str::to_owned),
            progress_seq: snapshot.progress_seq,
            seconds_since_progress: snapshot
                .last_progress_at
                .map(|t| now.duration_since(t).as_secs()),
            seconds_since_turn_start: snapshot
                .turn_started_at
                .map(|t| now.duration_since(t).as_secs()),
            stale: snapshot.stale,
        }
    }

    /// Tag the snapshot with its source project + session (same routing key as `agent://event`).
    pub fn with_route(mut self, project: &str, session: &str) -> Self {
        self.project = Some(project.to_owned());
        self.session = Some(session.to_owned());
        self
    }
}

/// Structured wire format of the task ledger (aligned with `TaskViewWire` in `web/src/types/wire.ts`).
#[derive(Debug, Clone, Serialize)]
pub struct TaskViewWire {
    pub goal: Option<String>,
    pub steps: Vec<TaskStepWire>,
    pub completed: usize,
    pub total: usize,
    pub current: Option<String>,
}

/// One step in the task ledger.
#[derive(Debug, Clone, Serialize)]
pub struct TaskStepWire {
    pub text: String,
    /// `pending` / `in_progress` / `completed` (serialized by `TodoStatus`).
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
    /// Translate an internal event into wire format. `MessageAdded` only exposes the
    /// role (the frontend does not consume the body; it is pushed by delta events).
    /// `project` / `session` are left empty here and tagged by [`Self::with_route`].
    ///
    /// **Reasoning (`ReasoningDelta`) returns `None`**: the reasoning process is
    /// consumed internally and not shown to the user, so it does not enter the wire
    /// format or reach the frontend. Returning `Option` lets the caller drop it.
    pub fn from_event(event: AgentEvent) -> Option<Self> {
        let wire = match event {
            AgentEvent::Retrying { attempt, reason } => Self::Retrying {
                project: None,
                session: None,
                attempt,
                reason,
            },
            // Reasoning does not reach the UI: swallow it (the SDK still emits the event for internal observation).
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
                error_kind,
                elapsed_ms,
            } => Self::ToolFinished {
                project: None,
                session: None,
                call_id,
                name,
                ok,
                output,
                error_kind: error_kind.map(error_kind_str),
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
            AgentEvent::Notice(text) => Self::Notice {
                project: None,
                session: None,
                text,
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

    /// Tag the event with its source project + session (the frontend routes by this
    /// with multiple projects and sessions).
    ///
    /// The project id is globally unique (canonical path); the session name is unique
    /// only within a project — together they form the frontend's routing key (matching
    /// the backend pump's `project::session` key).
    pub fn with_route(mut self, project: &str, session: &str) -> Self {
        let (project_slot, session_slot) = match &mut self {
            Self::Retrying {
                project, session, ..
            }
            | Self::ContentDelta {
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
            | Self::Notice {
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

/// One UI item in a session-view snapshot (aligned with `ItemWire` in `web/src/types/wire.ts`).
///
/// Mapped 1:1 from the core-layer [`CoreItemWire`] (`interface::session::ItemWire`):
/// when the UI subscribes to a session that is running / has accumulated history, it
/// first rebuilds the view from the snapshot, then layers subsequent events on top.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemWire {
    Message { role: String, content: String },
    Compact { content: String },
    Tools { calls: Vec<ToolCallWireDto> },
}

/// DTO for a single tool call in a snapshot.
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

/// UI role → wire-format string (aligned with `role` in `web/src/types/wire.ts`).
fn role_wire(role: crate::interface::view::Role) -> &'static str {
    use crate::interface::view::Role;
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Error => "error",
        Role::Notice => "notice",
    }
}

/// Session-view snapshot: the full rebuildable item list sent to the UI on subscribe.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshotWire {
    pub session: String,
    pub items: Vec<ItemWire>,
    /// Whether this session is currently running a turn (the frontend gates busy on this).
    pub running: bool,
    /// The session's current task-ledger snapshot (`None` = empty / no ledger).
    pub todo: Option<TaskViewWire>,
}

impl SessionSnapshotWire {
    /// Constructed from core-layer items (session name + item sequence + task-ledger snapshot).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::StateKeeper;

    /// `StateSnapshotWire` must be serialisable and **content-free**: the JSON keys are exactly
    /// the status fields, never any prompt / output / tool-argument text (the whole point of a
    /// separate `status://event` channel — `docs/agent-status.md` §6.1 / A5).
    #[test]
    fn state_snapshot_wire_is_content_free() {
        let keeper = StateKeeper::new();
        keeper.begin_turn();
        let wire = StateSnapshotWire::from_snapshot(&keeper.snapshot()).with_route("proj", "sess");
        let json = serde_json::to_value(&wire).unwrap();

        let obj = json.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "activity",
                "health",
                "health_detail",
                "liveness",
                "progress_seq",
                "project",
                "seconds_since_progress",
                "seconds_since_turn_start",
                "session",
                "stale",
            ]
        );
        // Routing + derived liveness are present; nothing content-bearing leaked in.
        assert_eq!(json["project"], "proj");
        assert_eq!(json["session"], "sess");
        assert_eq!(json["liveness"], "running");
        assert_eq!(json["health"], "healthy");
    }

    /// Every `Health` / `Liveness` / `Activity` variant maps to a stable lowercase tag (so the
    /// frontend can switch on it without parsing prose).
    #[test]
    fn state_snapshot_wire_maps_every_variant() {
        use crate::status::{Activity, Health, Liveness, StateSnapshot};
        let snap = |liveness, health, activity| StateSnapshot {
            liveness,
            health,
            activity,
            progress_seq: 7,
            last_progress_at: None,
            turn_started_at: None,
            stale: true,
        };

        let wire = StateSnapshotWire::from_snapshot(&snap(
            Liveness::Stalled,
            Health::SlowTool {
                name: "bash".into(),
                elapsed_ms: 4200,
            },
            Some(Activity::ToolCall),
        ));
        assert_eq!(wire.liveness, "stalled");
        assert_eq!(wire.health, "slow_tool");
        assert_eq!(wire.activity.as_deref(), Some("tool_call"));
        assert_eq!(wire.health_detail.as_deref(), Some("bash · 4200 ms"));
        assert_eq!(wire.progress_seq, 7);
        assert!(wire.stale);

        let wire = StateSnapshotWire::from_snapshot(&snap(
            Liveness::Unreachable,
            Health::Failed {
                kind: None,
                message: "boom".into(),
            },
            None,
        ));
        assert_eq!(wire.liveness, "unreachable");
        assert_eq!(wire.health, "failed");
        assert_eq!(wire.activity, None);
        assert_eq!(wire.health_detail.as_deref(), Some("boom"));

        let wire = StateSnapshotWire::from_snapshot(&snap(
            Liveness::Running,
            Health::Retrying {
                attempt: 2,
                reason: "429".into(),
            },
            Some(Activity::ModelCall),
        ));
        assert_eq!(wire.health, "retrying");
        assert_eq!(wire.health_detail.as_deref(), Some("attempt 2"));
    }
}
