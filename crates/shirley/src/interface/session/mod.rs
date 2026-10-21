//! Application-layer multi-session runtime container (`docs/multi-session.md` decision 4).
//!
//! This is the application layer's **single orchestration entry point**: it gathers three things in one place:
//! "where sessions come from" (`SessionCatalog`), "how far each session has run" ([`Session`]),
//! and "who is in the foreground" (`SessionManager::active`).
//!
//! Unlike [`crate::session`] (the storage / catalog layer): that layer only cares about "how the log files are read/written and
//! how sessions are listed"; this module cares about "what each session looks like in memory and who is in the foreground". A
//! [`Session`] holds its own independent [`Agent`] (decision 1: multiple sessions = multiple independent `Agent`s,
//! not "one Agent time-shared"), so different sessions' recall stores / task ledgers / session logs are naturally isolated.
//!
//! Identity lives on the container, `Agent` stays anonymous (decision 4): the factory `build_agent` creates an anonymous
//! `Agent`, and a `name` is stuck on it once when it is put into a `Session` -- avoiding "one id in `Agent`, one id in
//! `Session`" as two sources of truth.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use shirley_agent_sdk::{Agent, AgentEvent, Message, Usage};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::bootstrap::AgentFactory;
use crate::interface::view::{ChatMessage, Item, Role, ToolCallView, ToolGroup};
use crate::memory::MemoryRuntime;
use crate::session::{SessionCatalog, SessionStore};
use crate::skills::SkillRuntime;
use crate::status::StateKeeper;
use crate::subagent::{SubagentManager, SubagentStore};
use crate::todo::{TaskView, TodoStore};

/// Event fan-out channel capacity. A running subscriber that falls more than this far behind gets `Lagged`, and the subscriber (desktop
/// pump) skips and realigns by resending a snapshot (see `docs/multi-session.md`).
const EVENT_CHANNEL_CAPACITY: usize = 256;

fn proxy_session_id(workspace: &Path, name: Option<&str>) -> Option<String> {
    let name = name?;
    let identity = format!("{}::{name}", workspace.display());
    Some(Uuid::new_v5(&Uuid::NAMESPACE_URL, identity.as_bytes()).to_string())
}

/// One session = one runtime container.
///
/// It holds the `Agent` **exclusively** owned by that session and all "session-scoped" UI state (message items /
/// scroll position / usage / this-turn state ...). Switching the foreground only moves the [`SessionManager::active`]
/// pointer onto it -- **without rebuilding the `Agent`** (rebuilding would lose background context).
pub struct Session {
    pub model_selection: Option<String>,
    model_selection_path: Option<std::path::PathBuf>,
    model_providers_root: Option<std::path::PathBuf>,
    model_binding_error: Option<String>,
    /// Session name (reuses [`crate::session::SessionEntry::name`]); `None` means an in-memory session not yet persisted
    /// (lazy new, before materialization).
    pub name: Option<String>,
    /// The `Agent` exclusively owned by this session. It is `take`n out into a background task during a run, then put back
    /// afterward (same technique as the old `take_agent` / `restore_agent`, but **one per session**).
    pub agent: Option<Agent>,
    /// A **mirror** of this session's current model name.
    ///
    /// After the `Agent` is `take`n away during a run, `agent.model_config()` is no longer queryable (when desktop
    /// switches to another session and back, the running session is in exactly this state). The model picker / status line
    /// need to answer "what model does this session use" at **any** moment, so a copy is kept on the session. Write points:
    /// initialization in [`Session::new`], [`Session::begin_turn`] / [`Session::restore_agent`]
    /// syncing with the agent, and refreshed by [`Session::remember_model`] after switching/creating a session.
    model: Option<String>,
    /// This session's message items (the render view).
    pub items: Vec<Item>,
    /// Runtime status (liveness / health / activity / progress) -- the **single writer** of the
    /// session's run state. The former `waiting` / `waiting_since` / `compressing` / `running`
    /// flags are now derived from this keeper (see `docs/agent-status.md` §6.1).
    pub status: Arc<StateKeeper>,
    pub scroll: usize,
    pub auto_scroll: bool,
    pub max_scroll: usize,
    pub last_usage: Option<Usage>,
    pub total_usage: Usage,
    pub context_usage: Option<(u64, u64)>,
    /// The `items` index where this turn's streaming reply starts (used at wrap-up to truncate a half-finished streaming reply).
    pub streaming_delta_start: Option<usize>,
    /// The user pressed Esc to request interrupting the current reply (read by the main loop to trigger cancellation).
    pub interrupt_requested: bool,
    /// This session's event fan-out channel (multi-session: UI switches to a session -> subscribe, switches away -> unsubscribe).
    ///
    /// The channel is owned by the `Session` itself (symmetric with "each `Session` holds its own `Agent`", see
    /// `docs/multi-session.md` decision 2): `SessionManager` only feeds each
    /// `AgentEvent` to the corresponding session, it does not hold channels.
    events: broadcast::Sender<Result<AgentEvent, String>>,
    /// This session's persistent store (`None` = no persistence, for tests).
    ///
    /// Persistence is event-driven via [`Session::apply_event`]: on receiving the SDK's `MessageAdded` it
    /// `append`s. Session persistence is entirely in the application layer; the SDK no longer holds session logs.
    store: Option<Arc<dyn SessionStore>>,
    /// This session's memory runtime (`None` = no memory, for tests / no-config scenarios).
    ///
    /// One per session (`docs/memory.md` section 4.2): the query slot is independent, so concurrent multi-session runs do not interfere.
    /// The driver calls `set_query` before starting a turn (relevant retrieval based on this turn's user input); when the session ends
    /// it is handed to the curator.
    pub memory: Option<Arc<MemoryRuntime>>,
    /// This session's skill runtime (`None` = no skills, for tests / no-catalog scenarios).
    ///
    /// One per session (symmetric with `memory`): the activated set is isolated per session. The TUI's
    /// `/<skill-name>` slash entry activates directly through this handle (`docs/skills.md` 5.6),
    /// reusing the P1 `SkillContextProvider` injection seam -- no new injection path is added.
    pub skills: Option<Arc<SkillRuntime>>,
    /// This session's task ledger (`None` = no ledger, for tests / no-config scenarios).
    ///
    /// **Shares the same `Arc`** with the `todo` tool / `TodoContextProvider` (created and returned by `build_agent`),
    /// so the UI can read task progress in real time ([`TodoStore::view`]) -- the TUI reads it directly every frame in
    /// `draw`, and desktop reads it when the pump receives a `todo` tool completion event.
    pub todo: Option<Arc<TodoStore>>,
    /// This session's subagent manager (`None` = no subagents, for tests / child agents).
    ///
    /// **Shares the same `Arc`** with the `subagent` tool (created and returned by `build_agent`).
    /// It also holds the fork snapshot source (`parent_view`), fed here in [`Session::apply_event`]
    /// so a `fork` child inherits the parent's conversation (see `docs/subagent.md`).
    pub subagents: Option<Arc<SubagentManager>>,
    /// This session's permission broker (`None` = no permission layer, for tests / child agents).
    ///
    /// **Shares the same `Arc`** with the runtime's injected `Approver` (created by
    /// `build_agent`). The driver subscribes to it each turn to receive approval prompts and
    /// calls [`crate::permission::ApprovalBroker::resolve`] with the human's decision. The
    /// routing key (project/session) is set here once the session name is known.
    pub approval: Option<Arc<crate::permission::ApprovalBroker>>,
}

impl Session {
    /// Start a session with an existing `Agent`.
    ///
    /// `store` is this session's persistent store (`None` = no persistence); persistence goes through
    /// [`Session::apply_event`]'s event-driven path.
    #[expect(
        clippy::too_many_arguments,
        reason = "signature mirrors a public API; grouping would obscure the model-facing shape"
    )]
    pub fn new(
        mut agent: Agent,
        name: Option<String>,
        request_session_id: Option<String>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
        skills: Option<Arc<SkillRuntime>>,
        todo: Option<Arc<TodoStore>>,
        subagents: Option<Arc<SubagentManager>>,
        approval: Option<Arc<crate::permission::ApprovalBroker>>,
    ) -> Self {
        agent.set_request_session_id(request_session_id);
        // Route approval prompts to this session (the Agent is anonymous; identity lives here).
        if let Some(broker) = approval.as_ref() {
            broker.set_route(None, name.clone());
        }
        let model = Some(agent.model_config().model.clone());
        // Seed the fork snapshot source from the restored working set (system included; the fork
        // snapshot strips the leading `System` when it is actually used).
        if let Some(manager) = &subagents {
            manager.set_parent_view(agent.messages().to_vec());
            manager.set_parent_model(agent.model_config().clone());
        }
        Self {
            model_selection: None,
            model_selection_path: None,
            model_providers_root: None,
            model_binding_error: None,
            name,
            agent: Some(agent),
            model,
            items: Vec::new(),
            status: Arc::new(StateKeeper::new()),
            scroll: 0,
            auto_scroll: true,
            max_scroll: 0,
            last_usage: None,
            total_usage: Usage::default(),
            context_usage: None,
            streaming_delta_start: None,
            interrupt_requested: false,
            events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            store,
            memory,
            skills,
            todo,
            subagents,
            approval,
        }
    }

    /// Structured view of this session's task ledger (for the UI to render task progress); returns `None` for a missing / empty ledger.
    pub fn todo_view(&self) -> Option<TaskView> {
        self.todo.as_ref().and_then(|store| store.view())
    }

    /// This session's current model name.
    ///
    /// **Live `Agent` first, mirror fallback**: while the `Agent` is present (idle) it is authoritative (single source of truth);
    /// during a run, after the `Agent` is `take`n away it falls back to the [`Session::model`] mirror -- so when switching to another
    /// session and back, the model picker / status line can still show this session's model instead of blank.
    pub fn model_name(&self) -> Option<&str> {
        self.agent
            .as_ref()
            .map(|agent| agent.model_config().model.as_str())
            .or(self.model.as_deref())
    }

    /// Refresh the model-name mirror (called after changing the model on the `Agent`, or to sync when obtaining an `Agent` from outside).
    pub fn remember_model(&mut self) {
        if let Some(agent) = self.agent.as_ref() {
            self.model = Some(agent.model_config().model.clone());
            if let Some(manager) = &self.subagents {
                manager.set_parent_model(agent.model_config().clone());
            }
        }
    }

    fn attach_model_profile(&mut self, global: &Path, dir: Option<std::path::PathBuf>) {
        self.model_providers_root = Some(global.to_path_buf());
        self.model_selection_path = dir
            .zip(self.name.as_ref())
            .map(|(dir, name)| dir.join(format!("{name}.model.json")));
        let saved = self
            .model_selection_path
            .as_ref()
            .filter(|p| p.exists())
            .map(|path| {
                std::fs::read(path)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| {
                        serde_json::from_slice::<String>(&bytes).map_err(|e| e.to_string())
                    })
            });
        match saved {
            Some(Ok(key)) => {
                self.model_selection = Some(key.clone());
                match crate::model_providers::resolve(global, &key) {
                    Ok(Some(model)) => {
                        if let Some(agent) = &mut self.agent {
                            agent.set_model_config(model.config());
                        }
                        self.remember_model();
                    }
                    Ok(None) => {
                        self.model_binding_error =
                            Some("此聊天使用的服务商或模型已被移除，请重新选择模型".into());
                    }
                    Err(error) => self.model_binding_error = Some(error),
                }
            }
            Some(Err(error)) => {
                self.model_binding_error = Some(format!("聊天模型配置损坏：{error}"));
            }
            None => {
                self.model_selection = self.agent.as_ref().and_then(|agent| {
                    crate::model_providers::key_for_config(global, agent.model_config())
                });
                if let (Some(path), Some(key)) = (&self.model_selection_path, &self.model_selection)
                    && let Err(error) =
                        crate::memory::store::atomic_write(path, &serde_json::to_vec(key).unwrap())
                {
                    self.model_binding_error = Some(error.to_string());
                }
            }
        }
    }

    pub fn model_availability(&self) -> Result<(), String> {
        if let Some(error) = &self.model_binding_error {
            return Err(error.clone());
        }
        if let (Some(global), Some(key)) = (&self.model_providers_root, &self.model_selection)
            && crate::model_providers::resolve(global, key)?.is_none()
        {
            return Err("此聊天使用的服务商或模型已被移除，请重新选择模型".into());
        }
        Ok(())
    }

    pub fn select_model(
        &mut self,
        config: shirley_agent_sdk::ModelConfig,
        key: Option<String>,
    ) -> Result<(), String> {
        if self.agent.is_none() {
            return Err("对话正在执行，请结束后再切换模型".into());
        }
        if let Some(path) = &self.model_selection_path {
            match &key {
                Some(key) => crate::memory::store::atomic_write(
                    path,
                    &serde_json::to_vec(key).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?,
                None => match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.to_string()),
                },
            }
        }
        self.agent.as_mut().unwrap().set_model_config(config);
        self.model_selection = key;
        self.model_binding_error = None;
        self.remember_model();
        Ok(())
    }

    /// Subscribe to this session's event stream (multi-session: the UI subscribes when switching to this session, and drops the handle to unsubscribe when switching away).
    pub fn subscribe(&self) -> broadcast::Receiver<Result<AgentEvent, String>> {
        self.events.subscribe()
    }

    /// Publish one event to all subscribers (silently dropped when there are none -- the session still accumulates).
    pub fn publish(&self, event: Result<AgentEvent, String>) {
        let _ = self.events.send(event);
    }

    /// Accumulate one `AgentEvent` into this session's view (message items / usage / compression state / context).
    ///
    /// This is the accumulation entry **shared** by the TUI and desktop: whoever drives `run_stream` feeds the events
    /// here, and the UI only reads results. The `Err` branch is a runtime error message (rendered as an error item).
    pub fn apply_event(&mut self, event: Result<AgentEvent, String>) {
        // Status reduction happens here, once, for every interface (TUI / desktop / server): the
        // keeper is the **single writer** of liveness / health / activity / progress, so the
        // hand-maintained flags cannot drift from the event stream (docs/agent-status.md §6.1).
        match &event {
            Ok(event) => self.status.on_event(event),
            Err(error) => self.status.on_error(error),
        }
        match event {
            Ok(AgentEvent::MessageAdded(message)) => {
                // Keep the fork snapshot source current (the SDK gives a `Tool` no live message
                // handle, so the manager mirrors the parent's working set from here).
                if let Some(manager) = &self.subagents {
                    manager.push_parent_message(&message);
                }
                // Event-driven persistence: the SDK only emits events; persistence is the application layer's job (system does not go into the log).
                self.persist_message(&message);
                // The user message's view item is recorded by the driver when starting a turn (see `begin_turn`),
                // so here only the assistant / tool / summary views are added.
                if !matches!(&message, Message::User { .. }) {
                    if matches!(&message, Message::Assistant { .. }) {
                        self.finish_streaming_deltas();
                    }
                    self.add_message(message);
                }
            }
            Ok(AgentEvent::ContentDelta(delta)) => self.append_streaming_delta(delta),
            // Thinking (reasoning) is not displayed: digested internally, the user only cares about behavior and results.
            Ok(
                AgentEvent::ReasoningDelta(_)
                | AgentEvent::Retrying { .. }
                | AgentEvent::ToolStarted { .. }
                | AgentEvent::ToolFinished { .. },
            ) => {}
            // Neutral notice (e.g. silent image degradation): its own view item, muted, not
            // persisted to the session log (it is a per-turn UI hint, not conversation content).
            Ok(AgentEvent::Notice(text)) => self.push_message(Role::Notice, text),
            Ok(AgentEvent::Usage(usage)) => self.record_usage(usage),
            Ok(AgentEvent::CompressionStarted) => self.start_compression(),
            Ok(AgentEvent::CompressionFinished) => self.finish_compression(),
            Ok(AgentEvent::ContextUsage {
                used_tokens,
                limit_tokens,
            }) => self.record_context_usage(used_tokens, limit_tokens),
            Err(error) => self.add_error(error),
            Ok(AgentEvent::Finished(_)) => {
                self.streaming_delta_start = None;
                // Status (liveness / health / compressing) is reduced inside `apply_event`'s prologue;
                // the turn is released only after the driver returns the Agent (see `finish_turn`).
            }
        }
    }

    /// Normal completion, cancellation, and failure all clear the streaming start point, so the next turn's completion does not truncate the previous turn and new user input.
    pub fn finish_turn(&mut self) {
        self.streaming_delta_start = None;
        self.status.end_turn();
    }

    /// Current view snapshot: exports rebuildable plain data in UI item order (`Item` -> [`ItemWire`]).
    ///
    /// When the UI subscribes to a session that is **already running / has accumulated history**, it first takes a snapshot to rebuild the view, then overlays
    /// subsequent events -- avoiding "switching back shows a blank with only the increment".
    pub fn snapshot(&self) -> Vec<ItemWire> {
        self.items.iter().map(ItemWire::from_item).collect()
    }

    /// Rebuild view items from the current `Agent`'s message list (called after a session is loaded from disk).
    ///
    /// View items are a display projection of Agent messages; a session restored from disk already has its `Agent` carrying the full history,
    /// but `items` is still empty -- rebuild once so the snapshot / render has content.
    pub fn rebuild_items_from_agent(&mut self) {
        let items = self
            .agent
            .as_ref()
            .map(|agent| Self::items_from_messages(agent.messages()))
            .unwrap_or_default();
        self.items = items;
        self.streaming_delta_start = None;
        // Re-seed the fork snapshot source (e.g. after restoring a session from disk).
        if let (Some(manager), Some(agent)) = (&self.subagents, &self.agent) {
            manager.set_parent_view(agent.messages().to_vec());
        }
    }

    /// Rebuild view items from a persisted message (used when subscribing to a finished session mid-run / switching sessions).
    pub fn items_from_messages(messages: &[Message]) -> Vec<Item> {
        let mut items = Vec::new();
        for message in messages {
            match message {
                Message::User { content } => items.push(Item::Message(ChatMessage {
                    role: Role::User,
                    content: content.as_text(),
                })),
                Message::Assistant {
                    content,
                    tool_calls,
                    ..
                } => {
                    // Thinking (reasoning_content) is not projected into a view item -- the UI does not display thinking.
                    if let Some(content) = content.as_ref().filter(|text| !text.is_empty()) {
                        items.push(Item::Message(ChatMessage {
                            role: Role::Assistant,
                            content: content.clone(),
                        }));
                    }
                    if !tool_calls.is_empty() {
                        items.push(Item::Tools(ToolGroup {
                            calls: tool_calls
                                .iter()
                                .map(|call| ToolCallView {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                    arguments: call.arguments.clone(),
                                })
                                .collect(),
                        }));
                    }
                }
                Message::ContextSummary { content } => items.push(Item::Compact {
                    content: content.clone(),
                    expanded: false,
                }),
                Message::Tool { .. } | Message::System { .. } => {}
            }
        }
        items
    }

    pub(crate) fn push_message(&mut self, role: Role, content: String) {
        self.items
            .push(Item::Message(ChatMessage { role, content }));
    }

    fn append_streaming_delta(&mut self, delta: String) {
        if delta.is_empty() {
            return;
        }
        if self.streaming_delta_start.is_some() {
            if let Some(Item::Message(message)) = self.items.last_mut()
                && message.role == Role::Assistant
            {
                message.content.push_str(&delta);
                return;
            }
        } else {
            self.streaming_delta_start = Some(self.items.len());
        }
        self.push_message(Role::Assistant, delta);
    }

    fn finish_streaming_deltas(&mut self) {
        if let Some(start) = self.streaming_delta_start.take() {
            self.items.truncate(start);
        }
    }

    fn start_tool_calls(&mut self, calls: Vec<ToolCallView>) {
        if calls.is_empty() {
            return;
        }
        self.items.push(Item::Tools(ToolGroup { calls }));
    }

    pub(crate) fn add_message(&mut self, message: Message) {
        match message {
            Message::User { content } => self.push_message(Role::User, content.as_text()),
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                // Thinking (reasoning_content) does not become a view item -- the UI does not display thinking.
                if let Some(content) = content.filter(|c| !c.is_empty()) {
                    self.push_message(Role::Assistant, content);
                }
                if !tool_calls.is_empty() {
                    let calls = tool_calls
                        .into_iter()
                        .map(|call| ToolCallView {
                            id: call.id,
                            name: call.name,
                            arguments: call.arguments,
                        })
                        .collect();
                    self.start_tool_calls(calls);
                }
            }
            Message::ContextSummary { content } => self.items.push(Item::Compact {
                content,
                expanded: false,
            }),
            Message::Tool { .. } | Message::System { .. } => {}
        }
    }

    fn add_error(&mut self, error: String) {
        // A runtime error ends the turn and is classified structurally by the keeper (not by text).
        self.status.on_error(&error);
        self.push_message(Role::Error, error);
    }

    /// Append a message to the session log (event-driven persistence).
    ///
    /// A persistence failure is recorded as an error item but does not abort this turn: `docs/session.md` section 2.2 exposes persistence failure
    /// as an error, but the UI should not interrupt the whole turn because of it.
    fn persist_message(&mut self, message: &Message) {
        let Some(store) = self.store.clone() else {
            return;
        };
        if let Err(error) = store.append(message) {
            self.add_error(format!("failed to persist session: {error}"));
        }
    }

    /// Rewind the last user message: roll back both memory (`Agent`) and the disk log together.
    ///
    /// "Rewind the last turn" is a **session / UI business semantic** (who defines "a turn", whether the assistant / tool chain after it is
    /// dropped along with it), so the decision lives in the application layer; the SDK only provides the atomic capability
    /// [`Agent::truncate_messages`] (tail truncation + invariant maintenance). The log does not contain system,
    /// so the top one is subtracted when converting the length.
    pub fn rewind_last_user_turn(&mut self) -> Option<String> {
        let agent = self.agent.as_mut()?;
        let index = agent
            .messages()
            .iter()
            .rposition(|m| matches!(m, Message::User { .. }))?;
        let content = match &agent.messages()[index] {
            Message::User { content } => content.clone(),
            _ => unreachable!("rposition guarantees a User message"),
        };
        // Truncate to before that user message (dropping this turn's user and the assistant / tool chain after it).
        let new_len = agent.truncate_messages(index);
        let has_system = matches!(agent.messages().first(), Some(Message::System { .. }));
        let log_len = new_len.saturating_sub(usize::from(has_system));
        let store = self.store.clone();
        if let Some(store) = store
            && let Err(error) = store.truncate(log_len)
        {
            self.add_error(format!("failed to truncate session log: {error}"));
        }
        Some(content.as_text())
    }

    fn record_usage(&mut self, usage: Usage) {
        self.total_usage = self.total_usage + usage;
        self.last_usage = Some(usage);
    }

    #[expect(
        clippy::unused_self,
        reason = "event handlers share a uniform `&mut self` signature"
    )]
    fn start_compression(&mut self) {
        // Liveness/health for compaction is reduced by the keeper in `apply_event`'s prologue.
    }

    fn finish_compression(&mut self) {
        self.context_usage = None;
    }

    fn record_context_usage(&mut self, used_tokens: u64, limit_tokens: u64) {
        self.context_usage = Some((used_tokens, limit_tokens));
    }
}

/// Neutral wire format for UI items (`docs/multi-session.md` decision 4): mapped from [`Item`],
/// for the UI to rebuild the view when subscribing to a session that has accumulated history. The TUI does not consume it (it reads [`Item`] directly);
/// the desktop side converts it into a frontend DTO (`desktop::wire::ItemWire`).
#[derive(Debug, Clone)]
pub enum ItemWire {
    /// One message item (role / body).
    Message {
        role: Role,
        content: String,
    },
    Compact {
        content: String,
    },
    /// A group of tool calls.
    Tools {
        calls: Vec<ToolCallWire>,
    },
}

/// A single tool call inside [`ItemWire::Tools`].
#[derive(Debug, Clone)]
pub struct ToolCallWire {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ItemWire {
    /// Map from a UI item (`Item` -> wire format).
    pub fn from_item(item: &Item) -> Self {
        match item {
            Item::Compact { content, .. } => ItemWire::Compact {
                content: content.clone(),
            },
            Item::Message(message) => ItemWire::Message {
                role: message.role,
                content: message.content.clone(),
            },
            Item::Tools(group) => ItemWire::Tools {
                calls: group
                    .calls
                    .iter()
                    .map(|call| ToolCallWire {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .collect(),
            },
        }
    }
}

/// The globally unique session orchestrator: holds all [`Session`]s, the foreground pointer, plus the "factory that creates `Agent`s"
/// and the catalog of "where sessions come from".
///
/// When `factory` / `catalog` are `None` it degrades to "single session, no create/switch" -- for tests that do not care about
/// multi-session (such as `App::new`).
/// All inputs for one curation (shared by `spawn_curation` and `finish`).
struct CurationInputs {
    model_config: shirley_agent_sdk::ModelConfig,
    factory: Arc<AgentFactory>,
    memory: Arc<MemoryRuntime>,
    messages: Vec<Message>,
    session_ref: String,
}

pub struct SessionManager {
    sessions: HashMap<u64, Session>,
    /// Key of the foreground session.
    active: u64,
    next_id: u64,
    catalog: Arc<dyn SessionCatalog>,
    factory: Option<Arc<AgentFactory>>,
}

impl SessionManager {
    /// Start a manager containing only a single session with an existing `Agent` (no factory, cannot create/switch).
    #[expect(
        clippy::too_many_arguments,
        reason = "signature mirrors a public API; grouping would obscure the model-facing shape"
    )]
    pub fn single(
        agent: Agent,
        name: Option<String>,
        catalog: Arc<dyn SessionCatalog>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
        skills: Option<Arc<SkillRuntime>>,
        todo: Option<Arc<TodoStore>>,
        subagents: Option<Arc<SubagentManager>>,
        approval: Option<Arc<crate::permission::ApprovalBroker>>,
    ) -> Self {
        let mut sessions = HashMap::new();
        // Attach the `.agents/` sidecar before the session starts using the manager.
        {
            let dir = catalog.storage_dir();
            if let (Some(manager), Some(name), Some(dir)) =
                (subagents.as_ref(), name.as_deref(), dir)
            {
                manager.set_store(SubagentStore::for_session(&dir, name));
            }
        }
        sessions.insert(
            0,
            Session::new(
                agent, name, None, store, memory, skills, todo, subagents, approval,
            ),
        );
        Self {
            sessions,
            active: 0,
            next_id: 1,
            catalog,
            factory: None,
        }
    }

    /// A manager that can create `Agent`s: given a factory and session catalog, supports create / switch.
    #[expect(
        clippy::too_many_arguments,
        reason = "signature mirrors a public API; grouping would obscure the model-facing shape"
    )]
    pub fn with_factory(
        agent: Agent,
        name: Option<String>,
        catalog: Arc<dyn SessionCatalog>,
        factory: Arc<AgentFactory>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
        skills: Option<Arc<SkillRuntime>>,
        todo: Option<Arc<TodoStore>>,
        subagents: Option<Arc<SubagentManager>>,
        approval: Option<Arc<crate::permission::ApprovalBroker>>,
    ) -> Self {
        let mut sessions = HashMap::new();
        let request_session_id = proxy_session_id(&factory.working_dir, name.as_deref());
        {
            let dir = catalog.storage_dir();
            if let (Some(manager), Some(name), Some(dir)) =
                (subagents.as_ref(), name.as_deref(), dir)
            {
                manager.set_store(SubagentStore::for_session(&dir, name));
            }
        }
        sessions.insert(
            0,
            Session::new(
                agent,
                name,
                request_session_id,
                store,
                memory,
                skills,
                todo,
                subagents,
                approval,
            ),
        );
        sessions
            .get_mut(&0)
            .unwrap()
            .attach_model_profile(factory.model_providers_root(), catalog.storage_dir());
        Self {
            sessions,
            active: 0,
            next_id: 1,
            catalog,
            factory: Some(factory),
        }
    }

    /// Attach a `SubagentStore` (the session's `.agents/` sidecar) to a freshly built manager, so
    /// `subagent` persistence and the child-transcript preview work. No-op when the session has no
    /// name or the catalog has no storage directory (tests / single-session fallback).
    fn attach_subagent_store(&self, subagents: Option<&Arc<SubagentManager>>, name: Option<&str>) {
        let (Some(manager), Some(name), Some(dir)) = (subagents, name, self.catalog.storage_dir())
        else {
            return;
        };
        manager.set_store(SubagentStore::for_session(&dir, name));
    }

    /// Foreground session (read-only).
    pub fn active(&self) -> &Session {
        self.sessions
            .get(&self.active)
            .expect("active session must exist")
    }

    /// Foreground session (mutable).
    pub fn active_mut(&mut self) -> &mut Session {
        self.sessions
            .get_mut(&self.active)
            .expect("active session must exist")
    }

    /// Foreground session name.
    pub fn active_name(&self) -> Option<&str> {
        self.active().name.as_deref()
    }

    /// Session catalog handle (for the main loop to load the list / create-switch).
    pub fn catalog(&self) -> Arc<dyn SessionCatalog> {
        Arc::clone(&self.catalog)
    }

    /// The factory that creates `Agent`s (read-only handle). Returns `None` when there is no factory (single-session fallback).
    ///
    /// The UI uses it to convert the "selected model entry" into a full `ModelConfig`
    /// ([`AgentFactory::model_config_for`]), then issues it to the `Agent` in one shot
    /// (`docs/tool-calling-refactor.md` Step 3).
    pub fn factory(&self) -> Option<Arc<AgentFactory>> {
        self.factory.clone()
    }

    /// Whether a loaded session already has this name (used to reuse the existing `Agent` when switching, without rebuilding).
    fn id_of_name(&self, name: &str) -> Option<u64> {
        self.sessions
            .iter()
            .find(|(_, session)| session.name.as_deref() == Some(name))
            .map(|(id, _)| *id)
    }

    /// Insert a session with an existing `Agent`, returning its key (does not change the foreground pointer).
    #[expect(
        clippy::too_many_arguments,
        reason = "signature mirrors a public API; grouping would obscure the model-facing shape"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "signature mirrors a public API; grouping would obscure the model-facing shape"
    )]
    fn insert(
        &mut self,
        agent: Agent,
        name: Option<String>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
        skills: Option<Arc<SkillRuntime>>,
        todo: Option<Arc<TodoStore>>,
        subagents: Option<Arc<SubagentManager>>,
        approval: Option<Arc<crate::permission::ApprovalBroker>>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let request_session_id = self
            .factory
            .as_ref()
            .and_then(|factory| proxy_session_id(&factory.working_dir, name.as_deref()));
        self.attach_subagent_store(subagents.as_ref(), name.as_deref());
        self.sessions.insert(
            id,
            Session::new(
                agent,
                name,
                request_session_id,
                store,
                memory,
                skills,
                todo,
                subagents,
                approval,
            ),
        );
        if let Some(factory) = &self.factory {
            self.sessions
                .get_mut(&id)
                .unwrap()
                .attach_model_profile(factory.model_providers_root(), self.catalog.storage_dir());
        }
        id
    }

    /// Switch to the specified session (decision 4 / decision 9).
    ///
    /// Already loaded -> **only move the foreground pointer** (do not rebuild the `Agent`, preserving background context); not loaded ->
    /// `open` the log from the catalog, restore the working set via the factory's `build_agent`, then insert and switch to the foreground.
    /// Returns the session name.
    pub fn switch_to(&mut self, name: &str) -> Result<String, String> {
        // Leaving the foreground counts as this session's "turn ended" -- switching away triggers one incremental curation
        // (`docs/memory.md` section 5.1; the old session stays in memory and its `Agent` messages remain readable).
        let previous = self.active().name.clone();
        // Use the session-level query (live Agent first, mirror fallback) -- if the departing session is running, its `Agent`
        // has already been `take`n away, so reading `agent` directly would not get the model.
        let use_profile_default = self.factory.as_ref().is_some_and(|f| {
            crate::model_providers::default_config(f.model_providers_root())
                .ok()
                .flatten()
                .is_some()
        });
        let selected_model = (!use_profile_default)
            .then(|| self.active().model_name().map(str::to_owned))
            .flatten();
        if let Some(id) = self.id_of_name(name) {
            self.active = id;
            if let Some(previous) = previous.filter(|p| p != name) {
                self.spawn_curation(&previous);
            }
            return Ok(name.to_owned());
        }
        let factory = self
            .factory
            .as_ref()
            .ok_or_else(|| "the current session catalog does not support switching".to_owned())?;
        let store = self.catalog.open(name).map_err(|error| error.to_string())?;
        // Session persistence is in the application layer: load the log first, then hand it to the factory to create the `Agent` (system regenerated and placed on top).
        let log = store.load().map_err(|error| error.to_string())?;
        let mut built = factory
            .build_agent(log)
            .map_err(|error| error.to_string())?;
        if let Some(model) = selected_model {
            built.agent.set_model(model);
        }
        let id = self.insert(
            built.agent,
            Some(name.to_owned()),
            Some(store),
            Some(built.memory),
            Some(built.skills),
            Some(built.todo),
            built.subagents,
            Some(built.approval),
        );
        // A session restored from disk: rebuild view items from its history so the snapshot / render has content.
        if let Some(session) = self.sessions.get_mut(&id) {
            session.rebuild_items_from_agent();
        }
        self.active = id;
        if let Some(previous) = previous.filter(|p| p != name) {
            self.spawn_curation(&previous);
        }
        Ok(name.to_owned())
    }

    /// Create a (lazy) new session and switch to it, returning the new session name. An empty `title` = anonymous.
    ///
    /// Lazy: only names it now, does not persist; the file is actually created on the first message (`create_lazy`).
    pub fn create_new(&mut self, title: Option<&str>) -> Result<String, String> {
        // Same as `switch_to`: creating and switching away = the old session's turn ended, triggering one curation.
        let previous = self.active().name.clone();
        // Same as `switch_to`: session-level query, so the model is available even during a run.
        let use_profile_default = self.factory.as_ref().is_some_and(|f| {
            crate::model_providers::default_config(f.model_providers_root())
                .ok()
                .flatten()
                .is_some()
        });
        let selected_model = (!use_profile_default)
            .then(|| self.active().model_name().map(str::to_owned))
            .flatten();
        let factory = self
            .factory
            .as_ref()
            .ok_or_else(|| "the current session catalog does not support creating".to_owned())?;
        let (entry, store) = self
            .catalog
            .create_lazy(title)
            .map_err(|error| error.to_string())?;
        // The lazy session is empty now: load yields an empty working set; the file is materialized only when the first message is persisted.
        let log = store.load().map_err(|error| error.to_string())?;
        let mut built = factory
            .build_agent(log)
            .map_err(|error| error.to_string())?;
        if let Some(model) = selected_model {
            built.agent.set_model(model);
        }
        let id = self.insert(
            built.agent,
            Some(entry.name.clone()),
            Some(store),
            Some(built.memory),
            Some(built.skills),
            Some(built.todo),
            built.subagents,
            Some(built.approval),
        );
        self.active = id;
        if let Some(previous) = previous.filter(|p| p != entry.name.as_str()) {
            self.spawn_curation(&previous);
        }
        Ok(entry.name)
    }

    /// Delete a session (in-memory side). Refused while running (to avoid deleting a log being written).
    ///
    /// **Disk deletion is the caller's responsibility** (`SessionCatalog::delete`) -- this method only handles in-memory orchestration:
    /// remove the session from `sessions`; if the one deleted is exactly the foreground session, **create a lazy empty session and switch to it**
    /// (same UX as at startup: only names it now, does not persist; the file is created on the first message). This way, after deleting the current session
    /// the foreground pointer is not left dangling, and `active_name()` no longer returns the deleted name.
    ///
    /// **An unloaded session is allowed through directly**: the sidebar / list shows **all** sessions scanned from the catalog, but only those
    /// that have been opened are in memory. There is no in-memory orchestration to do for an unloaded session, and disk deletion is the caller's responsibility -- if
    /// this still reported "session does not exist", it would manifest as "you must open (select) it before deleting". Whether the name actually
    /// exists on disk is decided by the caller's `SessionCatalog::delete`.
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn delete_session(&mut self, name: &str) -> Result<(), String> {
        // Refused while running (to avoid deleting a log being written) -- `forget_session` includes this check.
        self.forget_session(name)
    }

    /// Make the manager **forget** the session named `name` (memory only, does not touch disk).
    ///
    /// Used for "delete session" (disk deletion is the caller's responsibility via [`SessionCatalog::delete`]) and
    /// "move session to another project" (disk migration is the caller's responsibility via [`SessionCatalog::move_to`]):
    /// both first remove the session from memory. Refused while running (to avoid removing a session being written).
    ///
    /// If the one removed is the **foreground session**, first create a lazy empty session and switch to it (same UX as at startup: only
    /// names it now, does not persist), so the foreground pointer is not dangling and `active_name()` no longer returns the removed name.
    /// **An unloaded session is allowed through directly** -- the list shows all sessions scanned from the catalog, and only those opened are
    /// in memory; there is no in-memory orchestration to do for an unloaded session.
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn forget_session(&mut self, name: &str) -> Result<(), String> {
        let Some(id) = self.id_of_name(name) else {
            return Ok(());
        };
        if self
            .sessions
            .get(&id)
            .is_some_and(|session| session.status.is_active())
        {
            return Err("agent is currently running".to_owned());
        }
        let was_active = id == self.active;
        if was_active {
            // The one removed is the foreground session: **first** add an empty session and switch to it (`create_new` moves `active`
            // to the new session), then remove the old one -- if the order were reversed, once `create_new` failed it would
            // leave a dangling `active` (a later `active()` would panic directly).
            self.create_new(None)?;
        }
        self.sessions.remove(&id);
        Ok(())
    }

    // ---- Runtime access by session name (driven by desktop's Tauri commands; the TUI uses Deref to access active) ----
    //
    // Key point: everything is fetched **by session name**, not depending on the `active` pointer -- when a background session is running the foreground may already
    // have switched away, and fetching by `active` would misattribute events / Agents to another session (the root cause of cross-session bugs).

    /// The session named `name` (read-only).
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn session(&self, name: &str) -> Option<&Session> {
        self.id_of_name(name).and_then(|id| self.sessions.get(&id))
    }

    /// The session named `name` (mutable).
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn session_mut(&mut self, name: &str) -> Option<&mut Session> {
        self.id_of_name(name)
            .and_then(|id| self.sessions.get_mut(&id))
    }

    /// Subscribe to the event stream of the session named `name`. Returns `None` if the session does not exist.
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn subscribe(&self, name: &str) -> Option<broadcast::Receiver<Result<AgentEvent, String>>> {
        self.session(name).map(Session::subscribe)
    }

    /// View snapshot of the session named `name` (returns `None` if the session does not exist).
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn snapshot(&self, name: &str) -> Option<Vec<ItemWire>> {
        self.session(name).map(Session::snapshot)
    }

    /// Start a turn: move out the `Agent` of the session named `name`, and optimistically record the **user message** into that session's
    /// view items (symmetric with the TUI's `App::submit`).
    ///
    /// Why record it here: `Session::apply_event` **intentionally ignores** `MessageAdded(User)`
    /// (see its comment) -- the user message is recorded by the **driver**, not the event stream. The TUI records it in `submit`,
    /// desktop's `agent_send` previously missed it, so `Session.items` never contained the user message, and
    /// when switching away and back the snapshot rebuild lost the user message (this function's regression test covers this).
    ///
    /// Returns `None` if the session does not exist or is running.
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn begin_turn(&mut self, name: &str, user_message: String) -> Option<Agent> {
        let session = self.session_mut(name)?;
        // Sync the current model name into the mirror first, then `take` the `Agent` away -- the mirror can still answer this session's model during the run.
        session.remember_model();
        let agent = session.agent.take()?;
        session.status.begin_turn();
        session.push_message(Role::User, user_message);
        Some(agent)
    }

    /// Put the `Agent` back into the session named `name`.
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn restore_agent(&mut self, name: &str, agent: Agent) {
        if let Some(session) = self.session_mut(name) {
            session.agent = Some(agent);
            session.remember_model();
            session.finish_turn();
        }
    }

    /// Rewind the last user turn of the session named `name`: drop the last user message
    /// **and every assistant / tool message after it** from both the in-memory `Agent` and
    /// the disk log, then remove the corresponding view items (the edited user item and
    /// everything after it). Returns the original user text so the UI can pre-fill an editor.
    ///
    /// This is the shared entry for the TUI (`/rewind` re-submit) and the desktop
    /// (`agent_resend`): "rewind the last turn" is a session / UI business semantic, so the
    /// orchestration lives here; the SDK only offers the atomic `Agent::truncate_messages`.
    ///
    /// Returns `None` when the session does not exist, is running, or has no user message.
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn rewind_last_turn(&mut self, name: &str) -> Option<String> {
        let session = self.session_mut(name)?;
        // Do not rewind while a turn is in flight (its `Agent` is out in a background task).
        if session.status.is_active() {
            return None;
        }
        let content = session.rewind_last_user_turn()?;
        // Drop the edited user item and every item after it (the old reply's view).
        if let Some(index) = session
            .items
            .iter()
            .rposition(|item| matches!(item, Item::Message(message) if message.role == Role::User))
        {
            session.items.truncate(index);
        }
        session.streaming_delta_start = None;
        Some(content)
    }

    /// Accumulate one event into the session named `name`, and fan it out to its subscribers.
    ///
    /// Called during a run by the task driving `run_stream` (`SessionManager` acts as the task executor).
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn apply_event(&mut self, name: &str, event: Result<AgentEvent, String>) {
        if let Some(session) = self.session_mut(name) {
            session.apply_event(event.clone());
            session.publish(event);
        }
    }

    /// Trigger one incremental curation when a session "ends" (`docs/memory.md` section 5.1).
    ///
    /// V1's "ended" determination: **leaving the foreground** (switching to another session / creating a session / deleting the foreground session).
    /// This is an **approximation** -- the user may have only temporarily switched away and back (recorded as a known gap, docs/memory.md
    /// section 11.4: more precise determinations such as exit / idle timeout are left for later). Triggering is best-effort: no
    /// memory, no messages, not in a tokio runtime (unit tests) -> silently skipped.
    ///
    /// Guarded by `Handle::try_current()`: curation is a background task and **never blocks** the synchronous
    /// session-switch path. Failures only log one stderr line and do not affect the main flow.
    fn spawn_curation(&self, name: &str) {
        let Some(inputs) = self.curation_inputs(name) else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(Self::run_curation(inputs));
    }

    /// Before exiting the program, run one curation for the **foreground session** and **wait for completion**.
    ///
    /// Complementary to [`Self::spawn_curation`] (triggered in the background on session switch): when a single session is chatted to the end and the program
    /// exits directly, the switch path never happens, so curation never triggers (`docs/memory.md` section 5.1 known
    /// gap). Exit is the most definite "turn ended" point, so one is added here. Uses `await` rather than
    /// `spawn`: after the main loop exits the tokio runtime shuts down immediately, and a `spawn`ed task may be discarded
    /// before it can finish -- on the exit path it must be awaited synchronously.
    pub async fn finish(&self) {
        if let Some(name) = self.active().name.clone()
            && let Some(inputs) = self.curation_inputs(&name)
        {
            Self::run_curation(inputs).await;
        }
    }

    /// Collect all inputs needed for one curation; returns `None` if the conditions are not met (no factory / no memory / no messages).
    /// Extracted so that [`Self::spawn_curation`] and [`Self::finish`] share the same
    /// pre-check and snapshot logic.
    fn curation_inputs(&self, name: &str) -> Option<CurationInputs> {
        let factory = self.factory.clone()?;
        let session = self.session(name)?;
        if session.model_availability().is_err() {
            return None;
        }
        // Only distill sessions "with content" (user + assistant turns); empty / single-message ones do not warrant a pointless model call.
        let messages: Vec<Message> = session
            .agent
            .as_ref()
            .map(|a| a.messages().to_vec())
            .unwrap_or_default();
        if messages.is_empty() {
            return None;
        }
        Some(CurationInputs {
            model_config: session.agent.as_ref()?.model_config().clone(),
            factory,
            // A session without a memory config is not distilled (no store to write to).
            memory: session.memory.clone()?,
            messages,
            session_ref: format!("{name}.jsonl"),
        })
    }

    /// The full pipeline for one curation: incremental extraction -> hit-count write-back -> periodic consolidation -> vector refresh.
    ///
    /// Best-effort throughout: failures only log one stderr line and are never propagated upward (curation is a background side job
    /// that should not affect the session itself).
    async fn run_curation(inputs: CurationInputs) {
        let CurationInputs {
            model_config,
            factory,
            memory,
            messages,
            session_ref,
        } = inputs;

        // Every id the curator persisted this round, uploaded to the cloud in one batch below.
        let mut written: Vec<String> = Vec::new();
        match factory.curate(&messages, &session_ref, &model_config).await {
            Ok(outcome) if !outcome.empty || !outcome.written.is_empty() => {
                eprintln!(
                    "[memory] curated `{session_ref}`: {} written, {} rejected",
                    outcome.written.len(),
                    outcome.rejected.len()
                );
                written.extend(outcome.written);
            }
            Ok(_) => {}
            Err(error) => eprintln!("[memory] curation for `{session_ref}` failed: {error}"),
        }
        // V2-D: write this session's accumulated retrieval hit counts back to the entries (usage_count / utility).
        match memory.flush_usage() {
            Ok(0) => {}
            Ok(n) => eprintln!("[memory] flushed usage for {n} entr(y/ies)"),
            Err(error) => eprintln!("[memory] usage flush failed: {error}"),
        }
        // V2-B/C: after incremental curation, trigger periodic consolidation by threshold (sleep learning).
        if factory.should_consolidate() {
            match factory.consolidate(&model_config).await {
                Ok(outcome) if !outcome.empty => {
                    eprintln!(
                        "[memory] consolidated: {} merged, {} superseded, {} requalified, {} rejected",
                        outcome.merged,
                        outcome.superseded,
                        outcome.requalified,
                        outcome.rejected.len()
                    );
                    written.extend(outcome.written);
                }
                Ok(_) => {}
                Err(error) => eprintln!("[memory] consolidation failed: {error}"),
            }
        }
        // The cloud owns memory: upload what we just wrote, then the cloud's authoritative record
        // set is installed back over the local mirror. A failed upload leaves the local copy in
        // place only as a transient cache; the next pull overwrites it either way.
        if let Some(global) = factory.memory_store().primary_root().parent() {
            match crate::remote::RemoteConfig::load(global) {
                Ok(remote) if !remote.url.is_empty() && remote.sync_personal_memory => {
                    match crate::memory::sync::push_written(global, &remote, &written).await {
                        Ok(report) => eprintln!(
                            "[memory:sync] pushed {} entr(y/ies) to the cloud, {} reconciled locally",
                            report.uploaded, report.downloaded
                        ),
                        Err(error) => eprintln!("[memory:sync] push failed: {error}"),
                    }
                }
                Ok(_) => {}
                Err(error) => eprintln!("[memory:sync] cloud config unreadable: {error}"),
            }
        }
        // V2.5: after both incremental / consolidation are persisted, embed new entries and clean orphan vectors, so the next hybrid retrieval
        // semantic leg is immediately usable (a no-op when embedding is not configured).
        match memory.refresh_vectors().await {
            Ok(0) => {}
            Ok(n) => eprintln!("[memory] embedded {n} entr(y/ies) into the vector index"),
            Err(error) => eprintln!("[memory] vector refresh failed: {error}"),
        }
    }

    /// Whether the **loaded** session named `name` is running (its `Agent` moved into a background task).
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn is_running(&self, name: &str) -> bool {
        self.session(name)
            .is_some_and(|session| session.status.is_active())
    }

    /// The foreground session's `Agent` (read-only). Returns `None` while running (moved into a background task).
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn active_agent(&self) -> Option<&Agent> {
        self.active().agent.as_ref()
    }

    /// The foreground session's current model name. **Also usable during a run** -- via [`Session::model_name`]'s mirror fallback,
    /// it does not depend on whether the `Agent` is present (the foreground session may be running when desktop switches away and back).
    #[cfg_attr(
        not(feature = "desktop"),
        allow(
            dead_code,
            reason = "only used when the relevant interface feature is enabled"
        )
    )]
    pub fn active_model_name(&self) -> Option<&str> {
        self.active().model_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::EmptySessionCatalog;
    use shirley_agent_sdk::{ModelConfig, ModelProtocol};

    #[test]
    fn proxy_session_identity_is_stable_and_project_scoped() {
        let first = proxy_session_id(Path::new("/repo/first"), Some("chat-1"));
        assert_eq!(
            first,
            proxy_session_id(Path::new("/repo/first"), Some("chat-1"))
        );
        assert_ne!(
            first,
            proxy_session_id(Path::new("/repo/second"), Some("chat-1"))
        );
        assert_ne!(
            first,
            proxy_session_id(Path::new("/repo/first"), Some("chat-2"))
        );
    }

    fn test_agent() -> Agent {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        Agent::builder().model_config(config).build().unwrap()
    }

    fn manager() -> SessionManager {
        SessionManager::single(
            test_agent(),
            Some("s".into()),
            Arc::new(EmptySessionCatalog),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    /// Regression: after desktop switches a session away and back, the user message must not be lost.
    ///
    /// The snapshot (from which the frontend rebuilds the transcript on switch-back) comes from `Session.items`; `apply_event`
    /// intentionally ignores `MessageAdded(User)`, so the user message must be recorded by the driver when starting a turn --
    /// `begin_turn` is exactly for this. Previously desktop's `agent_send` did not record it, the snapshot had no
    /// user message, and it "disappeared" on switch-back.
    #[test]
    fn begin_turn_records_user_message_into_snapshot() {
        let mut manager = manager();
        let agent = manager.begin_turn("s", "hello".into());
        assert!(
            agent.is_some(),
            "the first turn should be able to take the Agent"
        );
        assert!(
            manager.is_running("s"),
            "the session should be marked running after the Agent is taken"
        );

        let snapshot = manager.snapshot("s").unwrap();
        assert_eq!(
            snapshot.len(),
            1,
            "the snapshot should contain the just-recorded user message"
        );
        match &snapshot[0] {
            ItemWire::Message { role, content, .. } => {
                assert_eq!(*role, Role::User);
                assert_eq!(content, "hello");
            }
            other => panic!("expected a Message item, got {other:?}"),
        }

        // The same session must not start again while running (do not concurrently drive the same Agent).
        assert!(manager.begin_turn("s", "again".into()).is_none());
    }

    /// Regression: while a session is running (its `Agent` has been `take`n away) its model name can still be queried.
    ///
    /// When desktop switches to another session and back, the foreground session may be running -- at that point its `Agent`
    /// is not in the `Session`, and `active_agent()` returns `None`. Previously `agent_model_name`
    /// went through `active_agent().map(...).unwrap_or_default()`, so the frontend got an empty string and the model
    /// picker was cleared ("the model does not recover on switch-away and switch-back"). The mirror field `Session::model` must
    /// cover this query when the `Agent` is absent.
    #[test]
    fn model_name_survives_running_turn() {
        let mut manager = manager();
        assert_eq!(
            manager.active_model_name(),
            Some("test"),
            "reads the live Agent when idle"
        );

        let _agent = manager.begin_turn("s", "hi".into()).unwrap();
        assert!(manager.is_running("s"));
        assert_eq!(
            manager.active_model_name(),
            Some("test"),
            "the Agent is taken away during the run, the mirror should still answer this session's model"
        );
    }

    /// Regression: after a hot model switch the mirror syncs, and switching away and back still shows the new model.
    #[test]
    fn remember_model_tracks_hot_switch() {
        let mut manager = manager();
        manager
            .active_mut()
            .agent
            .as_mut()
            .unwrap()
            .set_model("other");
        manager.active_mut().remember_model();
        assert_eq!(manager.active_model_name(), Some("other"));
    }

    /// Regression: deleting an **unloaded** session (one that only exists on disk and was never opened) must be allowed through.
    ///
    /// The sidebar / list shows all sessions scanned from the catalog, and only those opened are in memory. Previously
    /// `delete_session` reported "session does not exist" directly for an unloaded session, so it had to be opened (selected)
    /// before it could be deleted -- this is exactly the root cause of "desktop must select before deleting". Whether it actually exists on disk is decided by the caller's
    /// `SessionCatalog::delete`; the in-memory side should not block it.
    #[test]
    fn delete_session_allows_unloaded_name() {
        let mut manager = manager();
        assert!(
            manager.delete_session("never-opened").is_ok(),
            "an unloaded session should be allowed through, left to the caller's disk-deletion decision"
        );
    }
}
