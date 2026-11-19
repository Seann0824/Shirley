//! Application-layer multi-session runtime container.
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

use shirley_agent_sdk::{Agent, AgentEvent, Message, MessageContent, Usage};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::bootstrap::AgentFactory;
use crate::interface::view::{
    ChatMessage, Item, RenderPart, Role, ToolCallView, ToolGroup, parts_from_message_content,
    parts_from_tool_value, user_display_text,
};
use crate::memory::MemoryRuntime;
use crate::session::{SessionCatalog, SessionStore};
use crate::status::StateKeeper;
use crate::subagent::{SubagentManager, SubagentStore};
use crate::todo::{TaskView, TodoStore};

/// Event fan-out channel capacity. A running subscriber that falls more than this far behind gets `Lagged`, and the subscriber (desktop
/// pump) skips and realigns by resending a snapshot.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Attach a tool result to its call in the view, matched by `call_id`.
///
/// Shared by the live path (`AgentEvent::ToolFinished`) and the restore path (`Message::Tool`), so
/// a rebuilt transcript keeps tool results instead of silently dropping them. Matching is by id,
/// not by position: tool calls of one round may finish out of order (they run concurrently).
fn attach_tool_result(
    items: &mut [Item],
    call_id: &str,
    ok: Option<bool>,
    output: String,
    parts: Vec<RenderPart>,
) {
    for item in items.iter_mut().rev() {
        let Item::Tools(group) = item else { continue };
        if let Some(call) = group.calls.iter_mut().find(|call| call.id == call_id) {
            call.finish(ok, output, parts);
            return;
        }
    }
}

/// True for a message that no provider will accept and no reader can use: an assistant turn with
/// neither text nor tool calls.
///
/// Such a message is produced when a round is **interrupted** (the user paused while the model was
/// streaming): the partial text has been discarded, so only the empty shell is left. Writing it to
/// the log would make the next request invalid, so it is dropped at the persistence boundary.
/// `Message::Tool` needs the same treatment in spirit, but an empty tool result is legal.
fn is_unsendable(message: &Message) -> bool {
    let Message::Assistant {
        content,
        reasoning_content,
        tool_calls,
    } = message
    else {
        return false;
    };
    if !tool_calls.is_empty() {
        return false;
    }
    let blank = |text: &Option<String>| text.as_deref().is_none_or(str::is_empty);
    blank(content) && blank(reasoning_content)
}

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
    pub last_usage: Option<Usage>,
    pub total_usage: Usage,
    pub context_usage: Option<(u64, u64)>,
    /// The `items` index where this turn's streaming reply starts (used at wrap-up to truncate a half-finished streaming reply).
    pub streaming_delta_start: Option<usize>,
    /// This session's event fan-out channel (multi-session: UI switches to a session -> subscribe, switches away -> unsubscribe).
    ///
    /// The channel is owned by the `Session` itself (symmetric with "each `Session` holds its
    /// own `Agent`"): `SessionManager` only feeds each
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
}

impl Session {
    /// Start a session with an existing `Agent`.
    ///
    /// `store` is this session's persistent store (`None` = no persistence); persistence goes through
    /// [`Session::apply_event`]'s event-driven path.
    pub fn new(
        mut agent: Agent,
        name: Option<String>,
        request_session_id: Option<String>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
        todo: Option<Arc<TodoStore>>,
        subagents: Option<Arc<SubagentManager>>,
    ) -> Self {
        agent.set_request_session_id(request_session_id);
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
            last_usage: None,
            total_usage: Usage::default(),
            context_usage: None,
            streaming_delta_start: None,
            events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            store,
            memory,
            todo,
            subagents,
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

    /// Hot-swap the model config **without** touching the model selection.
    ///
    /// Used by the composer's inline reasoning-effort control: effort is an attribute of the
    /// model already selected, so the model name -- and therefore the selection key its
    /// sidecar holds -- must stay exactly as it is. `select_model` would rewrite that sidecar
    /// (and clear it when the key is `None`), which is a different operation.
    pub fn hot_swap_model_config(
        &mut self,
        config: shirley_agent_sdk::ModelConfig,
    ) -> Result<(), String> {
        let agent = self
            .agent
            .as_mut()
            .ok_or_else(|| "对话正在执行，请结束后再调整".to_owned())?;
        agent.set_model_config(config);
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
                // An assistant message with neither text nor tool calls is not worth writing: it is
                // what an interrupted round leaves behind, and providers reject it outright
                // ("content or tool_calls must be set") -- which then poisons every later turn.
                if !is_unsendable(&message) {
                    self.persist_message(&message);
                }
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
                | AgentEvent::ToolStarted { .. },
            ) => {}
            // The tool result is recorded **in the view**, not only in the interface's live state:
            // `output` is what the model read, `parts` is what a human sees (docs/render-parts.md).
            // Without this arm a tool's result vanished on session switch / restart, because the
            // view was rebuilt from the message log and `Message::Tool` was skipped.
            Ok(AgentEvent::ToolFinished {
                call_id,
                ok,
                output,
                structured,
                ..
            }) => {
                let parts = parts_from_tool_value(structured.as_ref());
                self.finish_tool_call(&call_id, Some(ok), output, parts);
            }
            // Neutral notice (e.g. silent image degradation): its own view item, muted, not
            // persisted to the session log (it is a per-turn UI hint, not conversation content).
            Ok(AgentEvent::Notice(text)) => self.push_message(Role::Notice, text),
            Ok(AgentEvent::Usage(usage)) => self.record_usage(usage),
            Ok(AgentEvent::CompactionStarted) => self.start_compression(),
            Ok(AgentEvent::CompactionFinished) => self.finish_compression(),
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

    /// View snapshot as rebuildable plain data in UI item order (`Item` -> [`ItemWire`]): the UI
    /// takes one on open, then overlays subsequent events.
    ///
    /// Up to `limit` view items ending just before index `before` (the newest when `None`), and
    /// the index of the first one returned -- one page of the conversation, oldest first.
    pub fn page(&self, before: Option<usize>, limit: usize) -> (Vec<ItemWire>, usize) {
        let end = before.unwrap_or(self.items.len()).min(self.items.len());
        let start = end.saturating_sub(limit);
        (self.items[start..end].iter().map(ItemWire::from_item).collect(), start)
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
                    content: user_display_text(content),
                    parts: parts_from_message_content(content),
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
                            parts: Vec::new(),
                        }));
                    }
                    if !tool_calls.is_empty() {
                        items.push(Item::Tools(ToolGroup {
                            calls: tool_calls
                                .iter()
                                .map(|call| {
                                    ToolCallView::started(
                                        call.id.clone(),
                                        call.name.clone(),
                                        call.arguments.clone(),
                                    )
                                })
                                .collect(),
                        }));
                    }
                }
                Message::ContextSummary { content } => items.push(Item::Compact {
                    content: content.clone(),
                }),
                Message::Tool {
                    tool_call_id,
                    content,
                } => attach_tool_result(
                    &mut items,
                    tool_call_id,
                    None,
                    content
                        .as_ref()
                        .map(MessageContent::as_text)
                        .unwrap_or_default(),
                    Vec::new(),
                ),
                Message::System { .. } => {}
            }
        }
        items
    }

    pub(crate) fn push_message(&mut self, role: Role, content: String) {
        self.push_message_with_parts(role, content, Vec::new());
    }

    /// Push a message together with its render parts (empty parts = render `content` as-is).
    pub(crate) fn push_message_with_parts(
        &mut self,
        role: Role,
        content: String,
        parts: Vec<RenderPart>,
    ) {
        self.items.push(Item::Message(ChatMessage {
            role,
            content,
            parts,
        }));
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
            Message::User { content } => {
                // Project images too: `as_text()` alone is what silently dropped them on restore.
                let parts = parts_from_message_content(&content);
                self.push_message_with_parts(Role::User, user_display_text(&content), parts);
            }
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
                        .map(|call| ToolCallView::started(call.id, call.name, call.arguments))
                        .collect();
                    self.start_tool_calls(calls);
                }
            }
            Message::ContextSummary { content } => self.items.push(Item::Compact { content }),
            // Restore path: the log has the flattened output but not the status / parts. Attach the
            // text so a rebuilt view is not missing tool results; the live `ToolFinished` arm above
            // fills the rest (it runs right after this message in the same turn).
            Message::Tool {
                tool_call_id,
                content,
            } => {
                self.finish_tool_call(
                    &tool_call_id,
                    None,
                    content
                        .as_ref()
                        .map(MessageContent::as_text)
                        .unwrap_or_default(),
                    Vec::new(),
                );
            }
            Message::System { .. } => {}
        }
    }

    /// Record a tool result on its call in the view (matched by `call_id`).
    fn finish_tool_call(
        &mut self,
        call_id: &str,
        ok: Option<bool>,
        output: String,
        parts: Vec<RenderPart>,
    ) {
        attach_tool_result(&mut self.items, call_id, ok, output, parts);
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

    /// [`Self::rewind_last_user_turn`], returning the user message as sent and every message the
    /// rewind removed (the reply, its tool calls and their results).
    pub fn take_last_user_turn(&mut self) -> Option<(MessageContent, Vec<Message>)> {
        let agent = self.agent.as_mut()?;
        let index = agent
            .messages()
            .iter()
            .rposition(|m| matches!(m, Message::User { .. }))?;
        let content = match &agent.messages()[index] {
            Message::User { content } => content.clone(),
            _ => unreachable!("rposition guarantees a User message"),
        };
        let removed = agent.messages()[index + 1..].to_vec();
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
        Some((content, removed))
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

/// Neutral wire format for UI items: mapped from [`Item`],
/// for the UI to rebuild the view when subscribing to a session that has accumulated history. The TUI does not consume it (it reads [`Item`] directly);
/// the desktop side converts it into a frontend DTO (`desktop::wire::ItemWire`).
#[derive(Debug, Clone)]
pub enum ItemWire {
    /// One message item (role / body).
    Message {
        role: Role,
        content: String,
        /// Render parts (empty = the transport may skip the field; `content` is the text view).
        parts: Vec<RenderPart>,
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
    /// The tool's flattened output (`None` while running).
    pub output: Option<String>,
    /// Whether the call succeeded (`None` while running, or when restored from a log).
    pub ok: Option<bool>,
    /// Rich render parts (empty = render `output` as text).
    pub parts: Vec<RenderPart>,
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
                parts: message.parts.clone(),
            },
            Item::Tools(group) => ItemWire::Tools {
                calls: group
                    .calls
                    .iter()
                    .map(|call| ToolCallWire {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                        output: call.output.clone(),
                        ok: call.ok,
                        parts: call.parts.clone(),
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
    #[cfg(test)]
    pub fn single(
        agent: Agent,
        name: Option<String>,
        catalog: Arc<dyn SessionCatalog>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
        todo: Option<Arc<TodoStore>>,
        subagents: Option<Arc<SubagentManager>>,
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
            Session::new(agent, name, None, store, memory, todo, subagents),
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
        todo: Option<Arc<TodoStore>>,
        subagents: Option<Arc<SubagentManager>>,
    ) -> Self {
        let mut sessions = HashMap::new();
        let request_session_id = proxy_session_id(&crate::storage::global_root(), name.as_deref());
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
                todo,
                subagents,
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

    /// Whether a loaded session already has this name (used to reuse the existing `Agent` when switching, without rebuilding).
    fn id_of_name(&self, name: &str) -> Option<u64> {
        self.sessions
            .iter()
            .find(|(_, session)| session.name.as_deref() == Some(name))
            .map(|(id, _)| *id)
    }

    /// Insert a session with an existing `Agent`, returning its key (does not change the foreground pointer).
    fn insert(
        &mut self,
        agent: Agent,
        name: Option<String>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
        todo: Option<Arc<TodoStore>>,
        subagents: Option<Arc<SubagentManager>>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let request_session_id = self
            .factory
            .as_ref()
            .and_then(|_factory| proxy_session_id(&crate::storage::global_root(), name.as_deref()));
        self.attach_subagent_store(subagents.as_ref(), name.as_deref());
        self.sessions.insert(
            id,
            Session::new(
                agent,
                name,
                request_session_id,
                store,
                memory,
                todo,
                subagents,
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

    /// Create a background session **without touching the foreground pointer**.
    ///
    /// A task-pool job (`crate::pool`) runs in its own session, but it is not a conversation the
    /// user is looking at: creating it must not yank the sidebar selection out from under them, and
    /// it must not trigger the memory curation that a real switch implies. Everything else matches
    /// [`Self::create_new`] -- lazy on disk, same model as the current session.
    pub fn create_pool_session(&mut self, title: Option<&str>) -> Result<String, String> {
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
            .clone()
            .ok_or_else(|| "the current session catalog does not support creating".to_owned())?;
        let (entry, store) = self
            .catalog
            .create_lazy(title)
            .map_err(|error| error.to_string())?;
        let log = store.load().map_err(|error| error.to_string())?;
        let mut built = factory
            .build_worker_agent(log)
            .map_err(|error| error.to_string())?;
        if let Some(model) = selected_model {
            built.agent.set_model(model);
        }
        self.insert(
            built.agent,
            Some(entry.name.clone()),
            Some(store),
            Some(built.memory),
            Some(built.todo),
            built.subagents,
        );
        Ok(entry.name)
    }

    // ---- Runtime access by session name (driven by desktop's Tauri commands; the TUI uses Deref to access active) ----
    //
    // Key point: everything is fetched **by session name**, not depending on the `active` pointer -- when a background session is running the foreground may already
    // have switched away, and fetching by `active` would misattribute events / Agents to another session (the root cause of cross-session bugs).

    /// The session named `name` (read-only).
    pub fn session(&self, name: &str) -> Option<&Session> {
        self.id_of_name(name).and_then(|id| self.sessions.get(&id))
    }

    /// The session named `name` (mutable).
    pub fn session_mut(&mut self, name: &str) -> Option<&mut Session> {
        self.id_of_name(name)
            .and_then(|id| self.sessions.get_mut(&id))
    }

    /// Subscribe to the event stream of the session named `name`. Returns `None` if the session does not exist.
    pub fn subscribe(&self, name: &str) -> Option<broadcast::Receiver<Result<AgentEvent, String>>> {
        self.session(name).map(Session::subscribe)
    }

    /// One page of the session named `name` ([`Session::page`]); `None` if it does not exist.
    pub fn page(&self, name: &str, before: Option<usize>, limit: usize) -> Option<(Vec<ItemWire>, usize)> {
        self.session(name).map(|session| session.page(before, limit))
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
    /// `parts` are the message's non-text parts (attached images), so a snapshot taken while the
    /// turn runs shows them -- the `MessageAdded(User)` event that would carry them is ignored.
    pub fn begin_turn(
        &mut self,
        name: &str,
        user_message: String,
        parts: Vec<RenderPart>,
    ) -> Option<Agent> {
        let session = self.session_mut(name)?;
        // Sync the current model name into the mirror first, then `take` the `Agent` away -- the mirror can still answer this session's model during the run.
        session.remember_model();
        let agent = session.agent.take()?;
        session.status.begin_turn();
        session.push_message_with_parts(Role::User, user_message, parts);
        Some(agent)
    }

    /// Put the `Agent` back into the session named `name`.
    pub fn restore_agent(&mut self, name: &str, agent: Agent) {
        if let Some(session) = self.session_mut(name) {
            session.agent = Some(agent);
            session.remember_model();
            session.finish_turn();
        }
    }

    /// Take back the last user message of the session named `name` ("撤回"): the message and the
    /// reply to it are gone from the conversation, the model's context and the log. Returns the
    /// text as the user typed it, to edit, and the ids of tasks that turn dispatched.
    ///
    /// `None` when the session does not exist, is running, or has no user message.
    pub fn recall_last_turn(&mut self, name: &str) -> Option<Recalled> {
        let session = self.session_mut(name)?;
        if session.status.is_active() {
            return None;
        }
        let (content, removed) = session.take_last_user_turn()?;
        if let Some(index) = session
            .items
            .iter()
            .rposition(|item| matches!(item, Item::Message(message) if message.role == Role::User))
        {
            session.items.truncate(index);
        }
        session.streaming_delta_start = None;
        Some(Recalled {
            text: crate::interface::view::user_display_text(&content),
            dispatched: dispatched_tasks(&removed),
        })
    }

    /// Accumulate one event into the session named `name`, and fan it out to its subscribers.
    ///
    /// Called during a run by the task driving `run_stream` (`SessionManager` acts as the task executor).
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
    pub(crate) fn spawn_curation(&self, name: &str) {
        let Some(inputs) = self.curation_inputs(name) else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(Self::run_curation(inputs));
    }

    /// Collect all inputs needed for one curation; returns `None` if the conditions are not met (no factory / no memory / no messages).
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
    pub fn is_running(&self, name: &str) -> bool {
        self.session(name)
            .is_some_and(|session| session.status.is_active())
    }

    /// The foreground session's current model name. **Also usable during a run** -- via [`Session::model_name`]'s mirror fallback,
    /// it does not depend on whether the `Agent` is present (the foreground session may be running when desktop switches away and back).
    pub fn active_model_name(&self) -> Option<&str> {
        self.active().model_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::EmptySessionCatalog;
    use shirley_agent_sdk::{ContentPart, ImagePart, ModelConfig, ModelProtocol, ToolCall};

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

    #[test]
    fn tool_results_are_restored_into_the_view() {
        // The bug this pins: a rebuilt view used to skip `Message::Tool` entirely, so switching
        // sessions or restarting lost every tool result (they only existed in live events).
        let messages = vec![
            Message::User {
                content: MessageContent::text("read it"),
            },
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"a.txt\"}".into(),
                }],
            },
            Message::Tool {
                tool_call_id: "c1".into(),
                content: Some(MessageContent::text("file body")),
            },
        ];
        let items = Session::items_from_messages(&messages);
        let call = items
            .iter()
            .find_map(|item| match item {
                Item::Tools(group) => group.calls.first(),
                _ => None,
            })
            .expect("the tool call is part of the restored view");
        assert_eq!(call.output.as_deref(), Some("file body"));
        assert_eq!(
            call.ok, None,
            "the log carries no status, so it stays unknown"
        );
    }

    #[test]
    fn tool_results_attach_by_call_id_not_by_order() {
        // Calls of one round finish concurrently, so results arrive out of order.
        let mut items = vec![Item::Tools(ToolGroup {
            calls: vec![
                ToolCallView::started("c1".into(), "read".into(), "{}".into()),
                ToolCallView::started("c2".into(), "bash".into(), "{}".into()),
            ],
        })];
        attach_tool_result(&mut items, "c2", Some(true), "second".into(), Vec::new());
        attach_tool_result(&mut items, "c1", Some(false), "first".into(), Vec::new());
        // An unknown id must not panic or mismatch.
        attach_tool_result(&mut items, "nope", Some(true), "?".into(), Vec::new());
        let Item::Tools(group) = &items[0] else {
            unreachable!()
        };
        assert_eq!(group.calls[0].output.as_deref(), Some("first"));
        assert_eq!(group.calls[0].ok, Some(false));
        assert_eq!(group.calls[1].output.as_deref(), Some("second"));
    }

    #[test]
    fn user_images_survive_view_restore() {
        let content = MessageContent::parts(vec![
            ContentPart::Text {
                text: "what is this?".into(),
            },
            ContentPart::Image(ImagePart::url("data:image/png;base64,AAAA")),
        ]);
        let items = Session::items_from_messages(&[Message::User { content }]);
        let Item::Message(message) = &items[0] else {
            unreachable!()
        };
        assert_eq!(message.content, "what is this?");
        assert!(
            message
                .parts
                .iter()
                .any(|part| matches!(part, RenderPart::Image { .. })),
            "the image must be projected, not dropped: {:?}",
            message.parts
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
        )
    }

    /// Regression: after desktop switches a session away and back, the user message must not be lost.
    ///
    /// The snapshot (from which the frontend rebuilds the transcript on switch-back) comes from `Session.items`; `apply_event`
    /// intentionally ignores `MessageAdded(User)`, so the user message must be recorded by the driver when starting a turn --
    /// `begin_turn` is exactly for this. Previously desktop's `agent_send` did not record it, the snapshot had no
    /// user message, and it "disappeared" on switch-back.
    #[test]
    fn pages_walk_back_from_the_newest_items() {
        let mut manager = manager();
        let session = manager.session_mut("s").unwrap();
        for index in 0..5 {
            session.items.push(Item::Message(ChatMessage {
                role: Role::User,
                content: format!("m{index}"),
                parts: Vec::new(),
            }));
        }
        let text = |items: &[ItemWire]| -> Vec<String> {
            items
                .iter()
                .map(|item| match item {
                    ItemWire::Message { content, .. } => content.clone(),
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        };
        let (newest, start) = manager.page("s", None, 2).unwrap();
        assert_eq!((text(&newest), start), (vec!["m3".to_owned(), "m4".to_owned()], 3));
        let (older, start) = manager.page("s", Some(start), 2).unwrap();
        assert_eq!((text(&older), start), (vec!["m1".to_owned(), "m2".to_owned()], 1));
        let (first, start) = manager.page("s", Some(start), 2).unwrap();
        assert_eq!((text(&first), start), (vec!["m0".to_owned()], 0));
        assert!(manager.page("s", Some(0), 2).unwrap().0.is_empty());
    }

    #[test]
    fn begin_turn_records_user_message_into_snapshot() {
        let mut manager = manager();
        let agent = manager.begin_turn("s", "hello".into(), Vec::new());
        assert!(
            agent.is_some(),
            "the first turn should be able to take the Agent"
        );
        assert!(
            manager.is_running("s"),
            "the session should be marked running after the Agent is taken"
        );

        let (snapshot, start) = manager.page("s", None, usize::MAX).unwrap();
        assert_eq!(start, 0);
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
        assert!(
            manager
                .begin_turn("s", "again".into(), Vec::new())
                .is_none()
        );
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

        let _agent = manager.begin_turn("s", "hi".into(), Vec::new()).unwrap();
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

}

/// What [`SessionManager::recall_last_turn`] took back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recalled {
    /// The message as the user typed it (no attachment markers).
    pub text: String,
    /// Tasks the reply dispatched (`dispatch_task` results).
    pub dispatched: Vec<String>,
}

/// The task ids in the results of `dispatch_task` calls among `messages`.
fn dispatched_tasks(messages: &[Message]) -> Vec<String> {
    let calls: Vec<&str> = messages
        .iter()
        .flat_map(|message| match message {
            Message::Assistant { tool_calls, .. } => tool_calls
                .iter()
                .filter(|call| call.name == "dispatch_task")
                .map(|call| call.id.as_str())
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    messages
        .iter()
        .filter_map(|message| match message {
            Message::Tool {
                tool_call_id,
                content: Some(content),
            } if calls.contains(&tool_call_id.as_str()) => {
                serde_json::from_str::<serde_json::Value>(&content.as_text())
                    .ok()?
                    .get("task_id")?
                    .as_str()
                    .map(str::to_owned)
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod recall_tests {
    use super::*;
    use shirley_agent_sdk::ToolCall;

    #[test]
    fn tasks_dispatched_by_the_reply_are_found() {
        let call = |id: &str, name: &str| ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
        };
        let result = |id: &str, json: &str| Message::Tool {
            tool_call_id: id.into(),
            content: Some(MessageContent::text(json)),
        };
        let removed = vec![
            Message::Assistant {
                content: Some("好的，我来安排".into()),
                reasoning_content: None,
                tool_calls: vec![call("c1", "dispatch_task"), call("c2", "read"), call("c3", "dispatch_task")],
            },
            result("c1", r#"{"task_id":"t-1","title":"写周报"}"#),
            result("c2", r#"{"task_id":"not-a-dispatch"}"#),
            result("c3", "dispatch failed: no such bot"),
        ];
        assert_eq!(dispatched_tasks(&removed), ["t-1"]);
        assert!(dispatched_tasks(&[]).is_empty());
    }
}
