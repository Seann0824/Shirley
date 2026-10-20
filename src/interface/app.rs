use ratatui::layout::{Position, Rect};
use shirley_agent_sdk::{Agent, AgentEvent, Message, Usage};
use std::sync::Arc;

use crate::models::{ModelCatalog, ModelEntry, StaticCatalog};
use crate::session::{EmptySessionCatalog, SessionCatalog, SessionEntry};
use crate::subagent::SubagentInfo;
use crate::todo::TaskView;

use super::command::{CommandManager, CommandOutcome, DEFAULT_PREFIX};
use super::session::SessionManager;

use super::selection::Selection;
use super::ui::MessageCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    System,
    Error,
    /// A neutral system notice shown inline (e.g. "images were dropped, continuing"). Distinct
    /// from `System` (command output) and `Error`; rendered muted, no label, not markdown.
    Notice,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ToolCallView {
    pub id: String,
    pub name: String,
    // Raw JSON arguments, kept so the TUI can show which file was read
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ToolGroup {
    pub calls: Vec<ToolCallView>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Message(ChatMessage),
    Tools(ToolGroup),
    Compact { content: String, expanded: bool },
}

/// State of the `/model` picker. The list is fully loaded by the model catalog when opened,
/// so here we only store the data needed for display and the highlight position.
pub struct ModelPicker {
    pub entries: Vec<ModelEntry>,
    /// Index of the currently highlighted entry.
    pub selected: usize,
    /// The model value currently in use (for marking in the list), snapshotted once when opened.
    pub current: Option<String>,
}

/// State of the `/session` picker. The list is fully loaded by the session catalog when opened,
/// so here we only store the data needed for display and the highlight position.
///
/// The first item is fixed to the "+ New session" sentinel (`name` is the empty string), the rest are real sessions;
/// on confirm, `name` being empty routes to "new" or "switch".
pub struct SessionPicker {
    pub entries: Vec<SessionEntry>,
    /// Index of the currently highlighted entry.
    pub selected: usize,
    /// Current session name (for marking in the list), snapshotted once when opened.
    pub current: Option<String>,
}

impl SessionPicker {
    /// Name of the "new session" sentinel entry (the empty string marks the new-session entry point).
    pub const NEW_NAME: &'static str = "";
}

/// State of the `/subagents` picker (`docs/subagent.md` decision 10, P2).
///
/// The list is a snapshot of the current session's `SubagentManager` taken when the panel opens
/// (like `/session`, a local synchronous read, so no background load is needed). Confirming an
/// entry opens a read-only transcript overlay ([`SubagentPreview`]).
pub struct SubagentPicker {
    pub entries: Vec<SubagentInfo>,
    /// Index of the currently highlighted entry.
    pub selected: usize,
}

/// Read-only child-transcript overlay (decision 10: "click to preview the sub-session").
///
/// Holds the child's `Message` log as loaded from `<id>.jsonl` and a scroll offset. Rendering
/// reuses the parent path (`markdown::render`), so no separate view logic is built.
pub struct SubagentPreview {
    pub id: String,
    pub messages: Vec<Message>,
    /// Vertical scroll offset (lines), clamped at draw time.
    pub scroll: u16,
}

/// Current stage of the `/login` step-by-step flow.
///
/// Fixed order: first confirm the endpoint (base_url), then the key (api_key), and finally the model.
/// The endpoint comes first because the other two are meaningless without it; the model goes last because
/// after changing the endpoint one usually wants to set a new default model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStep {
    BaseUrl,
    ApiKey,
    Model,
}

impl LoginStep {
    /// How this step is asked in the prompt.
    fn prompt(&self) -> &'static str {
        match self {
            LoginStep::BaseUrl => "Enter the model service address base_url",
            LoginStep::ApiKey => "Enter the API Key (leave blank for local services; press Enter to skip)",
            LoginStep::Model => "Enter the default model name",
        }
    }
}

/// In-progress state of the `/login` step-by-step flow: collected fields + current stage.
///
/// Lives only in memory; cleared on `Esc` or when finished -- not persisted until complete, to avoid writing a half-finished config.
pub struct LoginFlow {
    pub step: LoginStep,
    /// The confirmed base_url (always present after the first step).
    pub base_url: String,
    /// The confirmed api_key (`None` means no auth).
    pub api_key: Option<String>,
}

pub struct App {
    /// Multi-session orchestrator: one independent `Agent` per session, `active` points to the foreground.
    /// In single-session mode (tests) there is only one session internally and `active` always points to it.
    sessions: SessionManager,
    exit: bool,
    input: String,
    input_cursor: usize,
    // History of sent prompts, for up/down key replay.
    history: Vec<String>,
    // None means not browsing history; Some(i) means history[i] is currently shown.
    history_index: Option<usize>,
    // The unsent input stashed before entering history browsing, restored when moving back down to the end.
    history_draft: String,
    // Whether to expand the full tool-call arguments (collapsed by default, showing only a one-line summary).
    show_tool_args: bool,
    pub(crate) message_cache: Option<MessageCache>,
    pub(crate) compact_hit_regions: Vec<(Rect, usize)>,
    /// Slash-command parser. Acts only on the TUI, never part of any context sent to the AI.
    commands: CommandManager,
    /// Fuzzy command suggestions triggered by `/` (e.g. `/inti` -> `init`). Empty means no suggestions now.
    suggestions: Vec<String>,
    /// Index of the currently highlighted suggestion (↑↓ / ←→ to move, Tab / Enter to accept).
    suggestion_index: usize,
    /// Model catalog: the model source for `/model`. Stable interface, swappable implementation (static / remote).
    ///
    /// Held via `Arc` to make it easy to move loading tasks to the background (a remote list must not block the UI).
    catalog: Arc<dyn ModelCatalog>,
    /// The open model picker; `None` means not open.
    picker: Option<ModelPicker>,
    /// Set when `/model` is triggered but the model list has not finished loading.
    /// Taken by the TUI main loop to load the catalog asynchronously (the list may come from a remote source).
    picker_requested: bool,
    /// The open session picker; `None` means not open.
    session_picker: Option<SessionPicker>,
    /// Set when `/session` is triggered but the session list has not finished loading.
    /// Taken by the TUI main loop to load the catalog and then open the picker.
    session_picker_requested: bool,
    /// The open subagent picker (`/subagents`); `None` means not open.
    subagent_picker: Option<SubagentPicker>,
    /// The open child-transcript preview overlay; `None` means not open.
    subagent_preview: Option<SubagentPreview>,
    /// The confirmed `/rewind` rewind point: the UI `items` index of the edited message.
    ///
    /// Rewind acts only on the **last user message** (`docs/session.md` decision 4),
    /// so only the UI index needs recording; the Agent-side rollback is done deterministically by `Session::rewind_last_user_turn`.
    /// On the next submit, roll back here first, then send as a brand-new turn.
    rewind_target: Option<usize>,
    /// Input-box hint override: a command-opened edit mode (e.g. rewind edit) uses it to say "Enter resend / Esc cancel".
    /// When empty, the default hint is used. Any ordinary input clears it.
    command_hint: Option<String>,
    /// In-progress `/login` step-by-step flow; `None` means not in login state.
    login: Option<LoginFlow>,
    /// Override for the target config file `/login` writes to; `None` uses the workspace default path.
    /// Its purpose is to let tests write to a temp directory instead of polluting the real workspace.
    config_path: Option<std::path::PathBuf>,
    /// Mouse drag selection; `None` means no text is currently selected.
    /// The `selection` module renders the highlight and extracts the text; releasing the left button copies.
    selection: Option<Selection>,
}

impl std::ops::Deref for App {
    type Target = super::session::Session;
    /// Transparent access to foreground-session fields: `self.items` / `self.agent` / `self.waiting` ...
    /// all resolve to `sessions.active()` ("the current session's view").
    fn deref(&self) -> &Self::Target {
        self.sessions.active()
    }
}

impl std::ops::DerefMut for App {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.sessions.active_mut()
    }
}

impl App {
    /// Construct with the built-in static model catalog (default).
    ///
    /// Mainly for tests and scenarios that do not care about catalog injection; the main program goes through [`App::with_manager`].
    #[allow(dead_code)]
    pub fn new(agent: Agent) -> Self {
        Self::with_catalog(agent, Arc::new(StaticCatalog::builtin()))
    }

    /// Construct with an injected model catalog. A remote catalog can be passed in here in the future; the UI needs no changes.
    ///
    /// The session catalog falls back to an empty implementation (nothing persisted, empty list), for scenarios that do not care about session switching.
    #[allow(dead_code)]
    pub fn with_catalog(agent: Agent, catalog: Arc<dyn ModelCatalog>) -> Self {
        Self::with_catalogs(agent, catalog, Arc::new(EmptySessionCatalog))
    }

    /// Construct with an injected model catalog and session catalog (single session, for tests).
    #[allow(dead_code)]
    pub fn with_catalogs(
        agent: Agent,
        catalog: Arc<dyn ModelCatalog>,
        session_catalog: Arc<dyn SessionCatalog>,
    ) -> Self {
        Self::with_manager(
            SessionManager::single(agent, None, session_catalog, None, None, None, None, None),
            catalog,
        )
    }

    /// Construct with an already-assembled multi-session orchestrator (the TUI / desktop go through here).
    pub fn with_manager(sessions: SessionManager, catalog: Arc<dyn ModelCatalog>) -> Self {
        Self {
            sessions,
            exit: false,
            input: String::new(),
            input_cursor: 0,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            show_tool_args: false,
            message_cache: None,
            compact_hit_regions: Vec::new(),
            commands: CommandManager::new(DEFAULT_PREFIX),
            suggestions: Vec::new(),
            suggestion_index: 0,
            catalog,
            picker: None,
            picker_requested: false,
            session_picker: None,
            session_picker_requested: false,
            subagent_picker: None,
            subagent_preview: None,
            rewind_target: None,
            command_hint: None,
            login: None,
            config_path: None,
            selection: None,
        }
    }

    pub fn last_usage(&self) -> Option<&Usage> {
        self.last_usage.as_ref()
    }

    pub fn total_usage(&self) -> &Usage {
        &self.total_usage
    }

    pub fn is_compressing(&self) -> bool {
        self.status.is_compacting()
    }

    pub fn context_usage(&self) -> Option<(u64, u64)> {
        self.context_usage
    }

    pub fn set_max_scroll(&mut self, max_scroll: usize) {
        // When the limit shrinks the old position may be out of range; clamp it back so we do not stop on a blank area
        self.max_scroll = max_scroll;
        if !self.auto_scroll && self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn auto_scroll(&self) -> bool {
        self.auto_scroll
    }

    // Negative is up. Scrolling up exits follow-the-bottom
    pub fn scroll_by(&mut self, delta: i32) {
        let max = self.max_scroll;
        // While following the bottom, self.scroll is stale (draw only computes the effective position locally each frame),
        // so use the bottom as the base; otherwise the first scroll-up would jump from the stale position.
        let base = if self.auto_scroll { max } else { self.scroll };
        let clamped = if delta < 0 {
            base.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            base.saturating_add(delta as usize).min(max)
        };
        self.scroll = clamped;
        self.auto_scroll = clamped >= max;
    }

    /// Begin a mouse drag selection (left button down).
    pub fn begin_selection(&mut self, position: Position) {
        self.selection = Some(Selection::new(position));
    }

    /// Extend the current selection to a new position (while dragging).
    pub fn extend_selection(&mut self, position: Position) {
        if let Some(selection) = self.selection.as_mut() {
            selection.extend(position);
        }
    }

    /// End the current selection (left button up): clear and return the selected content (a plain click returns `None`).
    ///
    /// Text extraction needs the buffer, so the caller (the main loop that has the buffer after rendering) handles it;
    /// here we only take the state away, to avoid an old selection lingering into the next drag.
    pub fn take_selection(&mut self) -> Option<Selection> {
        self.selection.take()
    }

    /// The current selection (for the render layer to highlight).
    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// Mouse-wheel scrolling: positive is down. Shares the same scrolling logic as the keyboard `PageUp`/`PageDown`.
    pub fn scroll_lines(&mut self, delta: i32) {
        self.scroll_by(delta);
    }

    pub fn should_exit(&self) -> bool {
        self.exit
    }

    pub fn exit(&mut self) {
        self.exit = true;
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    /// Byte offset of the input cursor, always on a character boundary.
    pub fn input_cursor(&self) -> usize {
        self.input_cursor
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn is_waiting(&self) -> bool {
        self.status.is_active()
    }

    pub fn waiting_seconds(&self) -> Option<u64> {
        self.status.elapsed_seconds()
    }

    pub fn show_tool_args(&self) -> bool {
        self.show_tool_args
    }

    pub fn toggle_latest_compact(&mut self) {
        if let Some(index) = self
            .items
            .iter()
            .rposition(|item| matches!(item, Item::Compact { .. }))
        {
            self.toggle_compact(index);
        }
    }

    pub fn toggle_compact_at(&mut self, position: Position) {
        if let Some(index) = self
            .compact_hit_regions
            .iter()
            .find_map(|(area, index)| area.contains(position).then_some(*index))
        {
            self.toggle_compact(index);
        }
    }

    fn toggle_compact(&mut self, index: usize) {
        if let Some(Item::Compact { expanded, .. }) = self.items.get_mut(index) {
            *expanded = !*expanded;
            self.message_cache = None;
        }
    }

    pub fn toggle_tool_args(&mut self) {
        self.show_tool_args = !self.show_tool_args;
        self.message_cache = None;
    }

    pub fn push_input(&mut self, ch: char) {
        // As soon as editing starts, exit history browsing, so replay and draft do not fight each other.
        self.history_index = None;
        self.input.insert(self.input_cursor, ch);
        self.input_cursor += ch.len_utf8();
        self.on_input_changed();
    }

    /// Paste: insert the whole text literally at the cursor.
    /// Newlines are normalized to `\n` and written as ordinary characters; sending is not triggered.
    pub fn insert_input(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.history_index = None;
        // The terminal may send CRLF; normalize to LF to avoid mixing `\r` into the input box.
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        self.input.insert_str(self.input_cursor, &normalized);
        self.input_cursor += normalized.len();
        self.on_input_changed();
    }

    /// Backspace: delete the character before the cursor.
    pub fn pop_input(&mut self) {
        if let Some((index, _)) = self.input[..self.input_cursor].char_indices().last() {
            self.input.remove(index);
            self.input_cursor = index;
            self.on_input_changed();
        }
    }

    /// Delete: delete the character at the cursor.
    pub fn delete_input(&mut self) {
        if self.input_cursor < self.input.len() {
            self.input.remove(self.input_cursor);
            self.on_input_changed();
        }
    }

    pub fn move_cursor_left(&mut self) {
        if let Some((index, _)) = self.input[..self.input_cursor].char_indices().last() {
            self.input_cursor = index;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if let Some(ch) = self.input[self.input_cursor..].chars().next() {
            self.input_cursor += ch.len_utf8();
        }
    }

    pub fn move_cursor_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.input_cursor = self.input.len();
    }

    pub fn submit(&mut self) -> Option<String> {
        // Login flow in progress: Enter submits "the value of the current field", not a prompt sent to the AI.
        // It takes precedence over all other branches -- in login state the input box is temporarily repurposed as a field editor.
        if self.login.is_some() {
            self.advance_login();
            return None;
        }
        if self.status.is_active() || self.agent.is_none() || self.input.trim().is_empty() {
            return None;
        }
        // Slash command: when it starts with the prefix and matches a built-in command, parse it first.
        // - Expand into a prompt: replace the input with the expanded content; the UI shows the expanded natural language,
        //   and the AI also receives only the prompt, with no difference from typing directly.
        // - Local system message: do not bother the AI; hint the user directly.
        // Skill names visible to the current session (dynamic slash entry). Fetch once and pass to the parser.
        let skill_names = self.skill_names();
        let outcome = self.commands.resolve(&self.input, &skill_names);
        match outcome {
            CommandOutcome::Prompt(expanded) => {
                self.input = expanded;
            }
            CommandOutcome::SystemMessage(message) => {
                self.input.clear();
                self.input_cursor = 0;
                self.add_system_message(message);
                self.suggestions.clear();
                return None;
            }
            CommandOutcome::ModelPicker => {
                // Do not send, do not record history: clear the input and ask the main loop to load the model list asynchronously.
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.command_hint = Some(" ↑↓ select · Enter confirm · Esc cancel ".to_owned());
                self.picker_requested = true;
                return None;
            }
            CommandOutcome::Rewind => {
                // Do not send, do not record history: rewind the last user message directly and put its original text back into the input box.
                // Option A (`docs/session.md` decision 4): only the latest one, so no selection panel is needed --
                // there is no "select" action, so a picker would be meaningless.
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.start_rewind_last_turn();
                return None;
            }
            CommandOutcome::SessionPicker => {
                // Do not send, do not record history: clear the input and ask the main loop to load the session list, then open the picker.
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.command_hint = Some(" ↑↓ select · Enter switch · Esc cancel ".to_owned());
                self.session_picker_requested = true;
                return None;
            }
            CommandOutcome::SubagentPicker => {
                // Do not send, do not record history: open the subagent picker from the current
                // session's manager snapshot (a local synchronous read, no background load needed).
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.open_subagent_picker();
                return None;
            }
            CommandOutcome::Login => {
                // Do not send, do not record history: enter step-by-step login; the input box becomes a field editor.
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.start_login();
                return None;
            }
            CommandOutcome::ActivateSkill { name, args } => {
                // Dynamic slash entry: the harness activates the skill directly (deterministic, skipping model judgment,
                // `docs/skills.md` 5.6). Activation is written to `SkillRuntime` and injected by `SkillContextProvider`
                // next turn -- no new injection seam is added.
                self.suggestions.clear();
                match self.activate_skill(&name) {
                    Ok(()) => {
                        if args.trim().is_empty() {
                            // No args: just activate, do not bother the model, and give a local system hint.
                            self.input.clear();
                            self.input_cursor = 0;
                            self.add_system_message(format!(
                                "Activated skill `{name}` (its instructions are injected every turn from now on)."
                            ));
                            return None;
                        }
                        // With args: after activation, send the args as this turn's prompt normally.
                        self.input = args;
                        self.input_cursor = self.input.len();
                    }
                    Err(reason) => {
                        // Activation failed (unknown skill / file read failure): report locally, do not send to the model.
                        self.input.clear();
                        self.input_cursor = 0;
                        self.add_system_message(format!("failed to activate skill `{name}`: {reason}"));
                        return None;
                    }
                }
            }
            CommandOutcome::Unknown => {}
        }

        // If in rewind-edit mode (`/rewind` not yet submitted), first synchronously roll back the Agent and the UI,
        // then send as a brand-new turn -- keeping the UI view and the real context always consistent.
        // The Agent side rolls back the last user message (and the assistant/tool chain after it); the UI side truncates to
        // before that item; the `push_message` of this turn then adds the edited content as a new item.
        if let Some(ui_index) = self.rewind_target.take() {
            // Roll back memory (Agent) and the disk log together to before that user message (done inside `Session`).
            self.rewind_last_user_turn();
            self.items.truncate(ui_index);
            self.streaming_delta_start = None;
            self.message_cache = None;
            self.command_hint = None;
        }

        self.command_hint = None;
        self.status.begin_turn();
        let prompt = std::mem::take(&mut self.input);
        self.input_cursor = 0;
        self.history.push(prompt.clone());
        self.history_index = None;
        self.history_draft.clear();
        // Record this turn's start point: on interrupt, roll back both the UI and the context based on it.
        self.interrupt_requested = false;
        self.push_message(Role::User, prompt.clone());
        self.suggestions.clear();
        Some(prompt)
    }

    /// Up key: replay an earlier history entry. On first entry, stash the current input.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(0) => 0,
            Some(index) => index - 1,
            None => {
                self.history_draft = self.input.clone();
                self.history.len() - 1
            }
        };
        self.history_index = Some(index);
        self.set_input(self.history[index].clone());
    }

    /// Down key: replay a later history entry; at the end, restore the input stashed before browsing.
    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.set_input(self.history[index + 1].clone());
        } else {
            self.history_index = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.set_input(draft);
        }
    }

    /// Replace the whole input content and put the cursor at the end.
    fn set_input(&mut self, text: String) {
        self.input = text;
        self.input_cursor = self.input.len();
    }

    pub fn take_agent(&mut self) -> Option<Agent> {
        if let Err(error) = self.sessions.active().model_availability() {
            self.add_system_message(error);
            self.sessions.active_mut().finish_turn();
            return None;
        }
        self.sessions.active_mut().agent.take()
    }

    pub fn restore_agent(&mut self, agent: Agent) {
        let session = self.sessions.active_mut();
        session.agent = Some(agent);
        // Returning the Agent means the turn is over: release the runtime status here too
        // (idempotent with `complete_turn` -> `finish_turn` -> `status.end_turn()`).
        session.status.end_turn();
    }

    /// Wrap-up after this turn's reply ends (completed normally or interrupted by the user).
    ///
    /// On user interrupt, **keep** the messages produced this turn (the user message + the partial streaming reply),
    /// as context reference for the next turn, without rolling back or deleting them (requirement 2).
    /// Keep the draft typed during the wait, and wait for the user to start the next turn.
    pub fn complete_turn(&mut self) {
        self.sessions.active_mut().finish_turn();
        self.interrupt_requested = false;
        self.message_cache = None;
    }

    // ---- Interrupt the current reply ----

    /// The user pressed Esc to request interrupting the current reply (only effective while waiting for a reply).
    pub fn request_interrupt(&mut self) {
        if self.status.is_active() {
            self.interrupt_requested = true;
        }
    }

    /// Whether an interrupt has been requested (the main loop uses this to send a cancel signal to the running task).
    pub fn interrupt_requested(&self) -> bool {
        self.interrupt_requested
    }

    // ---- History rewind (`/rewind`, option A: only roll back the latest user message) ----

    /// The landing point of `/rewind`: directly rewind the **last** user message and put its original text back into the input box for editing and resending.
    ///
    /// Only the latest one (`docs/session.md` decision 4) -- there is no "select" action,
    /// so no selection panel is needed: find the last user message and enter edit mode.
    /// Compressed-away history is not in `agent.messages`, so it is naturally not rewindable, and the semantics are self-consistent.
    ///
    /// Here it **only enters edit mode** (records the rewind point + fills the input box); the actual rollback is deferred to `submit`:
    /// this way the UI keeps the original conversation during editing, and the render layer can draw the edited item in place as an input box
    /// (see `ui.rs::draw_rewind_edit`), so the user can see the context.
    pub fn start_rewind_last_turn(&mut self) {
        let Some(agent) = self.agent.as_ref() else {
            return;
        };
        // The original text of the last user message on the Agent side.
        let Some(content) = agent.messages().iter().rev().find_map(|m| match m {
            Message::User { content } => Some(content.clone()),
            _ => None,
        }) else {
            self.add_system_message("no message to rewind to.".to_owned());
            return;
        };
        // The UI-side index of the last user item: one-to-one with the Agent user messages (each submit produces one).
        let Some(ui_index) = self
            .items
            .iter()
            .rposition(|item| matches!(item, Item::Message(m) if m.role == Role::User))
        else {
            self.add_system_message("no message to rewind to.".to_owned());
            return;
        };

        self.rewind_target = Some(ui_index);
        self.set_input(content.as_text());
        self.message_cache = None;
        // Enter "edit and resend" mode: Enter resends, Esc cancels this edit.
        // The input box is drawn in place at that message's position by the render layer (see `ui.rs::draw_rewind_edit`).
        self.command_hint = Some(" Enter resend · Esc cancel this edit ".to_owned());
    }

    /// Input-box hint override for a command edit mode (`None` if none).
    pub fn command_hint(&self) -> Option<&str> {
        self.command_hint.as_deref()
    }

    /// Whether in "rewind edit and resend" mode (already in edit mode, waiting to submit after editing).
    pub fn is_rewind_edit(&self) -> bool {
        self.rewind_target.is_some()
    }

    /// The UI item index being edited in rewind-edit mode (the render layer draws that message in place as an input box).
    pub fn rewind_edit_item(&self) -> Option<usize> {
        self.rewind_target
    }

    /// Test-only: enter rewind-edit mode directly and point at a UI item.
    #[cfg(test)]
    pub fn set_rewind_edit_for_test(&mut self, ui_index: usize) {
        self.rewind_target = Some(ui_index);
    }

    /// Cancel this rewind edit: clear the edit state and the input; neither the Agent nor the UI is changed.
    /// The landing point for the user "cancelling without pressing Enter".
    pub fn cancel_rewind_edit(&mut self) {
        if self.rewind_target.take().is_some() {
            self.input.clear();
            self.input_cursor = 0;
            self.command_hint = None;
            self.suggestions.clear();
        }
    }

    // ---- Step-by-step login (`/login`) ----
    //
    // Step-by-step Q&A instead of a form overlay: reuse the existing input box and `submit` chain, no new render state needed.
    // The cost is one Enter per step; the benefit is not touching the UI layer and being immediately usable (see the conversation log).

    /// Whether in the `/login` step-by-step flow (the render layer switches the input-box hint based on this).
    pub fn is_login(&self) -> bool {
        self.login.is_some()
    }

    /// Override `/login`'s persistence target (defaults to the global config).
    /// Lets tests inject a temp path to avoid polluting the real workspace.
    #[cfg(test)]
    pub fn set_config_path(&mut self, path: impl Into<std::path::PathBuf>) {
        self.config_path = Some(path.into());
    }

    /// Current stage of `/login` (for the UI / tests to inspect).
    #[cfg(test)]
    pub fn login_step(&self) -> Option<LoginStep> {
        self.login.as_ref().map(|flow| flow.step)
    }

    /// Enter step-by-step login: default to the current config and start by asking for base_url.
    ///
    /// The input box is pre-filled with the current value; pressing Enter directly means "keep unchanged"; edit to change.
    pub fn start_login(&mut self) {
        let (base_url, api_key) = match self.agent.as_ref() {
            Some(agent) => {
                let config = agent.model_config();
                (config.base_url.clone(), config.api_key.clone())
            }
            None => (String::new(), None),
        };
        self.login = Some(LoginFlow {
            step: LoginStep::BaseUrl,
            base_url,
            api_key,
        });
        // Pre-fill the current base_url; Enter keeps it.
        self.prompt_login_step();
    }

    /// Cancel login: clear the flow and the input; neither the Agent nor the config is changed. The landing point for the user pressing Esc.
    pub fn cancel_login(&mut self) {
        if self.login.take().is_some() {
            self.input.clear();
            self.input_cursor = 0;
            self.command_hint = None;
            self.suggestions.clear();
            self.add_system_message("login cancelled.".to_owned());
        }
    }

    /// Fill the current stage's default value into the input box and update the hint.
    fn prompt_login_step(&mut self) {
        let Some(step) = self.login.as_ref().map(|flow| flow.step) else {
            return;
        };
        let current = match step {
            LoginStep::BaseUrl => self
                .login
                .as_ref()
                .map(|flow| flow.base_url.clone())
                .unwrap_or_default(),
            LoginStep::ApiKey => self
                .login
                .as_ref()
                .and_then(|flow| flow.api_key.clone())
                .unwrap_or_default(),
            LoginStep::Model => self
                .agent
                .as_ref()
                .map(|agent| agent.model_config().model.clone())
                .unwrap_or_default(),
        };
        self.set_input(current);
        self.command_hint = Some(format!(" {} · Enter confirm · Esc cancel ", step.prompt()));
        self.add_system_message(format!("{} (Enter keeps the current value):", step.prompt()));
    }

    /// Submit the current field and advance to the next step; when finished, persist and hot-update.
    fn advance_login(&mut self) {
        let Some(mut flow) = self.login.take() else {
            return;
        };
        let value = std::mem::take(&mut self.input);
        self.input_cursor = 0;
        self.suggestions.clear();

        match flow.step {
            LoginStep::BaseUrl => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    // An empty endpoint is meaningless: stay on this step and re-ask, do not advance.
                    self.add_system_message("base_url cannot be empty, please enter again.".to_owned());
                    self.login = Some(flow);
                    self.prompt_login_step();
                    return;
                }
                flow.base_url = trimmed.to_owned();
                flow.step = LoginStep::ApiKey;
                self.login = Some(flow);
                self.prompt_login_step();
            }
            LoginStep::ApiKey => {
                let trimmed = value.trim();
                // Empty input = no auth (common for local services), different from "keep the old value" -- here it is explicitly cleared.
                flow.api_key = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                flow.step = LoginStep::Model;
                self.login = Some(flow);
                self.prompt_login_step();
            }
            LoginStep::Model => {
                let trimmed = value.trim();
                let model = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                self.finish_login(&flow, model);
            }
        }
    }

    /// Persist + hot-update: write the global config and make the Agent immediately use the new endpoint.
    fn finish_login(&mut self, flow: &LoginFlow, model: Option<String>) {
        self.command_hint = None;
        self.message_cache = None;

        // Hot-update the Agent: the endpoint / key take effect immediately (the model is optional; if blank, the current one is kept).
        // The protocol is re-decided at the same entry point and **issued in one shot** -- switching away from a content endpoint returns to the config default,
        // with no residue (`docs/tool-calling-refactor.md` Step 3).
        let config = self.sessions.factory().map(|factory| {
            factory.model_config_for_login(
                flow.base_url.clone(),
                flow.api_key.clone(),
                model.clone(),
            )
        });
        if let Some(config) = config {
            if let Err(error) = self.sessions.active_mut().select_model(config, None) { self.add_system_message(error); return; }
        } else if let Some(agent) = self.agent.as_mut() {
            agent.set_provider(flow.base_url.clone(), flow.api_key.clone());
            if let Some(model) = model.clone() {
                agent.set_model(model);
            }
        }

        // Persist: write the global config (read, modify, then write; other keys untouched);
        // tests can override it to a temp path (see `set_config_path`).
        let target = self
            .config_path
            .clone()
            .unwrap_or_else(crate::storage::config_path);
        let provider = crate::settings::ProviderSettings {
            protocol: None,
            base_url: Some(flow.base_url.clone()),
            api_key: flow.api_key.clone(),
            models_url: None,
            model: model.clone(),
            context_window_tokens: None,
            supports_images: None,
            // `/login` only changes the endpoint / key / model; thinking and the escape hatch stay as-is (None = do not override).
            thinking: None,
            reasoning_effort: None,
            extra_body: None,
            tool_mode: None,
        };
        match crate::settings::save_provider(&target, provider) {
            Ok(path) => {
                let model_note = model.map(|m| format!(", model {m}")).unwrap_or_default();
                self.add_system_message(format!(
                    "logged in: {} (written to {}){}",
                    flow.base_url,
                    path.display(),
                    model_note
                ));
            }
            Err(error) => {
                // The hot-update already took effect; it just was not persisted -- tell the user honestly, do not pretend success.
                self.add_system_message(format!(
                    "switched service ({}), but writing the config failed: {error}",
                    flow.base_url
                ));
            }
        }
    }

    /// Accumulate one `AgentEvent` into the current foreground session (shared `Session::apply_event`).
    ///
    /// The TUI and desktop go through the same accumulation path: event handling is only here, and the UI only reads results.
    /// `Err` is a runtime error message, rendered as an error item.
    pub fn apply_event(&mut self, event: Result<AgentEvent, String>) {
        self.sessions.active_mut().apply_event(event);
        self.message_cache = None;
    }

    /// Append a message item (and invalidate the render cache).
    fn push_message(&mut self, role: Role, content: String) {
        self.sessions.active_mut().push_message(role, content);
        self.message_cache = None;
    }

    /// Rebuild items from a persisted message (for tests and history replay; `apply_event` goes through `Session` internally).
    pub fn add_message(&mut self, message: Message) {
        self.sessions.active_mut().add_message(message);
        self.message_cache = None;
    }

    /// TUI-local system hint (e.g. command feedback), not part of the context sent to the AI.
    pub fn add_system_message(&mut self, content: String) {
        self.push_message(Role::System, content);
    }

    /// Refresh the fuzzy command suggestions from the current input. Only offered when the input starts with the prefix and has no exact match.
    /// Cleared when there are no suggestions. Called after every input edit.
    pub fn refresh_suggestions(&mut self) {
        let skill_names = self.skill_names();
        self.suggestions = self
            .commands
            .fuzzy_match(&self.input, &skill_names)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        // The suggestion set changed, so the highlight returns to the first item.
        self.suggestion_index = 0;
    }

    /// Skill names visible to the current session (candidates for the dynamic slash entry).
    ///
    /// The catalog is shared across sessions -> names are constant; fetched here from the foreground session's `SkillRuntime`.
    fn skill_names(&self) -> Vec<String> {
        self.sessions
            .active()
            .skills
            .as_ref()
            .map(|runtime| runtime.skill_names())
            .unwrap_or_default()
    }

    /// Activate a skill on the current session's `SkillRuntime` (slash entry).
    ///
    /// Reuses the P1 runtime: activation only writes the runtime set; the body is **appended** by `SkillContextProvider`
    /// when assembling the next request -- `render()`'s append semantics keep the KV prefix stable.
    fn activate_skill(&self, name: &str) -> Result<(), String> {
        let runtime = self
            .sessions
            .active()
            .skills
            .as_ref()
            .ok_or_else(|| "skills are not enabled for the current session".to_owned())?;
        runtime
            .activate(name)
            .map(|_| ())
            .map_err(|error| match error {
                crate::skills::runtime::ActivateError::UnknownSkill(name) => {
                    format!("unknown skill `{name}`")
                }
                crate::skills::runtime::ActivateError::ReadFailed(reason) => reason,
            })
    }

    /// Current command suggestions (sorted by match quality).
    pub fn suggestions(&self) -> &[String] {
        &self.suggestions
    }

    /// Index of the currently highlighted suggestion (the render layer draws `▶` based on it).
    pub fn suggestion_index(&self) -> usize {
        self.suggestion_index
    }

    /// Move the suggestion highlight (`delta` is -1 / +1). Clamped to `[0, len-1]`, no wrap at the ends.
    /// No-op when there are no suggestions.
    pub fn suggestion_move(&mut self, delta: isize) {
        if self.suggestions.is_empty() {
            return;
        }
        let last = self.suggestions.len() - 1;
        let next = self.suggestion_index as isize + delta;
        self.suggestion_index = next.clamp(0, last as isize) as usize;
    }

    /// Dismiss the current suggestion popup (Esc): clears only the suggestions, leaving the input itself untouched.
    pub fn dismiss_suggestions(&mut self) {
        self.suggestions.clear();
        self.suggestion_index = 0;
    }

    /// Tab / Enter completion: accept the **currently highlighted** suggestion, replace the input with `/<name> ` and clear the suggestions,
    /// then continue editing the arguments.
    pub fn accept_suggestion(&mut self) {
        let index = self
            .suggestion_index
            .min(self.suggestions.len().saturating_sub(1));
        if let Some(name) = self.suggestions.get(index).cloned() {
            self.input = format!("{}{} ", DEFAULT_PREFIX, name);
            self.input_cursor = self.input.len();
            self.suggestions.clear();
            self.suggestion_index = 0;
        }
    }

    // ---- Model picker (`/model`) ----

    /// The main loop takes away the "need to load the model list" request.
    pub fn take_picker_request(&mut self) -> bool {
        std::mem::take(&mut self.picker_requested)
    }

    /// Model catalog handle (for the main loop to move into a background task for async loading). Stable interface, swappable implementation.
    pub fn catalog(&self) -> Arc<dyn ModelCatalog> {
        Arc::clone(&self.catalog)
    }

    /// Open the picker with a loaded model list, highlighting the model currently in use.
    pub fn open_picker(&mut self, entries: Vec<ModelEntry>) {
        if entries.is_empty() {
            self.command_hint = None;
            self.add_system_message("no models available.".to_owned());
            return;
        }
        let current = self.current_model();
        let selected = current
            .as_ref()
            .and_then(|value| entries.iter().position(|e| &e.value == value))
            .unwrap_or(0);
        self.picker = Some(ModelPicker {
            entries,
            selected,
            current,
        });
        self.message_cache = None;
    }

    /// Current model value (from the Agent config).
    pub fn current_model(&self) -> Option<String> {
        self.agent
            .as_ref()
            .map(|agent| agent.model_config().model.clone())
    }

    pub fn picker(&self) -> Option<&ModelPicker> {
        self.picker.as_ref()
    }

    /// Move the picker highlight up/down.
    pub fn picker_move(&mut self, delta: i32) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let len = picker.entries.len() as i32;
        if len == 0 {
            return;
        }
        let next = (picker.selected as i32 + delta).rem_euclid(len);
        picker.selected = next as usize;
    }

    /// Confirm the selection: switch the Agent's model and close the panel. Returns the selected model value.
    pub fn picker_confirm(&mut self) -> Option<String> {
        let picker = self.picker.take()?;
        self.message_cache = None;
        self.command_hint = None;
        let entry = picker.entries.get(picker.selected).cloned()?;
        // Compute a full `ModelConfig` from the model entry (model value + tool protocol) and **issue it in one shot** --
        // same semantics as desktop's `agent_set_model` (`docs/tool-calling-refactor.md` Step 3).
        // Without a factory (single-session fallback / tests), it degrades to changing only the model name.
        let config = self
            .sessions
            .factory()
            .map(|factory| factory.model_config_for(&entry));
        if let Some(config) = config {
            let key = entry.configured.as_ref().map(|_| entry.value.clone());
            if let Err(error) = self.sessions.active_mut().select_model(config, key) { self.add_system_message(error); return None; }
        } else if let Some(agent) = self.agent.as_mut() {
            agent.set_model(entry.value.clone());
        }
        self.add_system_message(format!("switched model: {}", entry.label));
        Some(entry.value)
    }

    /// Cancel the selection.
    pub fn picker_cancel(&mut self) {
        if self.picker.take().is_some() {
            self.message_cache = None;
        }
        self.command_hint = None;
    }

    /// Whether the picker is open (the main loop routes keys to the panel instead of the input box based on this).
    pub fn is_picker_open(&self) -> bool {
        self.picker.is_some()
    }

    // ---- Session picker (`/session`) ----

    /// The main loop takes away the "need to load the session list" request.
    pub fn take_session_picker_request(&mut self) -> bool {
        std::mem::take(&mut self.session_picker_requested)
    }

    /// Session catalog handle (for the main loop to load the list).
    pub fn session_catalog(&self) -> Arc<dyn SessionCatalog> {
        self.sessions.catalog()
    }

    /// Run one memory curation for the foreground session before exiting the program (`docs/memory.md` section 5.1).
    ///
    /// Delegated to [`SessionManager::finish`]: when a single session is chatted to the end and the program exits directly, the switch path never happens,
    /// so this is the only moment "the turn ends" can trigger curation. Best-effort; failures are only logged.
    pub async fn finish(&self) {
        self.sessions.finish().await;
    }

    /// Current (foreground) session name (for status display).
    pub fn current_session(&self) -> Option<&str> {
        self.sessions.active_name()
    }

    /// Task ledger view of the current (foreground) session (for the status line to render task progress); returns `None` for a missing / empty ledger.
    pub fn task_view(&self) -> Option<TaskView> {
        self.sessions.active().todo_view()
    }

    /// Open the picker with a loaded session list, highlighting the current session.
    ///
    /// The first item is fixed to the "+ New session" sentinel, so "switch" and "new" happen in the same panel.
    pub fn open_session_picker(&mut self, mut entries: Vec<SessionEntry>) {
        let current = self.sessions.active_name().map(str::to_owned);
        // Put the sentinel on top: an empty name means "new".
        let mut all = vec![SessionEntry {
            name: SessionPicker::NEW_NAME.to_owned(),
            label: "+ New session".to_owned(),
            preview: String::new(),
            modified_ms: 0,
            turns: 0,
        }];
        all.append(&mut entries);
        let selected = current
            .as_ref()
            .and_then(|name| all.iter().position(|e| &e.name == name))
            .unwrap_or(0);
        self.session_picker = Some(SessionPicker {
            entries: all,
            selected,
            current,
        });
        self.message_cache = None;
    }

    pub fn session_picker(&self) -> Option<&SessionPicker> {
        self.session_picker.as_ref()
    }

    /// Move the picker highlight up/down (no wrap; stops at the ends -- the list contains the new-session entry, and wrapping is easy to mis-trigger).
    pub fn session_picker_move(&mut self, delta: i32) {
        let Some(picker) = self.session_picker.as_mut() else {
            return;
        };
        let last = picker.entries.len().saturating_sub(1) as i32;
        let next = (picker.selected as i32 + delta).clamp(0, last);
        picker.selected = next as usize;
    }

    /// Confirm the selection: create or switch to the target session, rebuilding the UI and the Agent context. Returns a result message.
    ///
    /// Returning `Some(message)` means a switch happened; `None` means the panel was not open or the switch failed
    /// (on failure a system message has already been shown).
    pub fn session_picker_confirm(&mut self) -> Option<String> {
        let picker = self.session_picker.take()?;
        self.message_cache = None;
        self.command_hint = None;
        let entry = picker.entries.get(picker.selected).cloned()?;

        // Orchestration is handed to `SessionManager`: both new and switch only move the `active` pointer,
        // **without rebuilding the Agent** (an already-loaded session reuses its ready Agent; an unloaded one is restored via the factory).
        let result = if entry.name == SessionPicker::NEW_NAME {
            self.sessions.create_new(None)
        } else {
            self.sessions.switch_to(&entry.name)
        };
        let name = match result {
            Ok(name) => name,
            Err(error) => {
                self.add_system_message(format!("failed to switch session: {error}"));
                return None;
            }
        };
        // Session changed: replay the new session's history in the UI and reset session-bound statistics.
        self.rebuild_items_from_agent();
        self.reset_session_stats();
        let message = format!("switched to session: {name}");
        self.add_system_message(message.clone());
        Some(message)
    }

    /// Cancel the selection.
    pub fn session_picker_cancel(&mut self) {
        if self.session_picker.take().is_some() {
            self.message_cache = None;
        }
        self.command_hint = None;
    }

    /// Whether the session picker is open (the main loop routes keys to the panel instead of the input box based on this).
    pub fn is_session_picker_open(&self) -> bool {
        self.session_picker.is_some()
    }

    // ---- Subagent picker + transcript preview (`/subagents`, docs/subagent.md decision 10) ----

    /// Open the subagent picker from the current session's `SubagentManager` snapshot.
    ///
    /// A local synchronous read (the registry is in memory), so unlike `/model` / `/session`
    /// there is no background load. If the session has no subagent manager (or no subagents yet)
    /// a system hint is shown instead of an empty panel.
    pub fn open_subagent_picker(&mut self) {
        let entries = self
            .sessions
            .active()
            .subagents
            .as_ref()
            .map(|manager| manager.list())
            .unwrap_or_default();
        if entries.is_empty() {
            self.add_system_message("No subagents in this session yet.".to_owned());
            return;
        }
        self.subagent_picker = Some(SubagentPicker {
            entries,
            selected: 0,
        });
        self.message_cache = None;
    }

    pub fn subagent_picker(&self) -> Option<&SubagentPicker> {
        self.subagent_picker.as_ref()
    }

    /// Move the subagent picker highlight (no wrap, stops at the ends).
    pub fn subagent_picker_move(&mut self, delta: i32) {
        let Some(picker) = self.subagent_picker.as_mut() else {
            return;
        };
        let last = picker.entries.len().saturating_sub(1) as i32;
        let next = (picker.selected as i32 + delta).clamp(0, last);
        picker.selected = next as usize;
    }

    /// Confirm the highlighted subagent: load its transcript from the store and open the read-only
    /// preview overlay. Returns the opened id, or `None` if nothing was open / the transcript
    /// could not be loaded (a system hint is shown in that case).
    pub fn subagent_picker_confirm(&mut self) -> Option<String> {
        let picker = self.subagent_picker.take()?;
        self.message_cache = None;
        let entry = picker.entries.get(picker.selected)?.clone();
        let Some(manager) = self.sessions.active().subagents.clone() else {
            self.add_system_message("No subagent manager for this session.".to_owned());
            return None;
        };
        let messages = manager.load_transcript(&entry.id);
        self.subagent_preview = Some(SubagentPreview {
            id: entry.id.clone(),
            messages,
            scroll: 0,
        });
        Some(entry.id)
    }

    /// Cancel the subagent picker (back to the input box).
    pub fn subagent_picker_cancel(&mut self) {
        if self.subagent_picker.take().is_some() {
            self.message_cache = None;
        }
    }

    /// Whether the subagent picker is open (the main loop routes keys here first).
    pub fn is_subagent_picker_open(&self) -> bool {
        self.subagent_picker.is_some()
    }

    /// The open child-transcript preview overlay, if any.
    pub fn subagent_preview(&self) -> Option<&SubagentPreview> {
        self.subagent_preview.as_ref()
    }

    /// Whether the preview overlay is open (the main loop routes keys here first).
    pub fn is_subagent_preview_open(&self) -> bool {
        self.subagent_preview.is_some()
    }

    /// Scroll the preview overlay. The upper clamp is applied at draw time (the exact content
    /// height depends on the wrap width), so a too-large value here is harmless.
    pub fn subagent_preview_scroll(&mut self, delta: i32) {
        let Some(preview) = self.subagent_preview.as_mut() else {
            return;
        };
        preview.scroll = (preview.scroll as i32 + delta).max(0) as u16;
    }

    /// Close the preview overlay (back to the input box).
    pub fn subagent_preview_close(&mut self) {
        if self.subagent_preview.take().is_some() {
            self.message_cache = None;
        }
    }

    /// Cancel the subagent shown in the preview (`c` key): abort it if it is still running and
    /// refresh the overlay's transcript snapshot so the new status is reflected. Best-effort.
    pub fn subagent_preview_cancel(&mut self) {
        let Some(id) = self.subagent_preview.as_ref().map(|p| p.id.clone()) else {
            return;
        };
        let Some(manager) = self.sessions.active().subagents.clone() else {
            return;
        };
        if let Err(error) = manager.cancel(&id) {
            self.add_system_message(format!("failed to cancel subagent {id}: {error}"));
            return;
        }
        // Reload the transcript snapshot so the preview reflects the cancellation immediately.
        let messages = manager.load_transcript(&id);
        if let Some(preview) = self.subagent_preview.as_mut() {
            preview.messages = messages;
        }
    }

    /// Replay UI items from the Agent's current message list (called after switching sessions).
    ///
    /// UI items are a view of the Agent messages; switching sessions swaps the whole history, so the view must be rebuilt entirely,
    /// not incrementally. `add_message` naturally skips system / tool (consistent with the display scope).
    pub fn rebuild_items_from_agent(&mut self) {
        self.items.clear();
        self.streaming_delta_start = None;
        self.scroll = 0;
        self.auto_scroll = true;
        let messages: Vec<Message> = self
            .agent
            .as_ref()
            .map(|agent| agent.messages().to_vec())
            .unwrap_or_default();
        for message in messages {
            self.add_message(message);
        }
        self.message_cache = None;
    }

    /// Reset session-bound statistics (usage / context usage) after switching sessions.
    fn reset_session_stats(&mut self) {
        self.last_usage = None;
        self.total_usage = Usage::default();
        self.context_usage = None;
        // Runtime status lives in each session's own keeper; switching sessions simply exposes a
        // different keeper, so there is no stale flag to clear here.
    }

    /// Recompute suggestions on input change (called uniformly after push/insert/pop/delete/move etc.).
    pub fn on_input_changed(&mut self) {
        // Both rewind-edit mode and login mode keep their own hints -- what the user is changing is "the content to resend" or
        // "the field being filled in", and both states still hold; otherwise any input change restores the default hint.
        if !self.is_rewind_edit() && !self.is_login() {
            self.command_hint = None;
        }
        self.refresh_suggestions();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shirley_agent_sdk::{ModelConfig, ModelProtocol};

    fn app() -> App {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        App::new(Agent::builder().model_config(config).build().unwrap())
    }

    /// Test App with a factory (switching models goes through `factory.model_config_for` for a one-shot issue).
    /// Assemble the factory with a unique temp workspace to avoid concurrent tests polluting each other.
    fn app_with_factory(tag: &str) -> App {
        let root = std::env::temp_dir().join(format!(
            "shirley_app_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&root);
        let factory = std::sync::Arc::new(
            crate::bootstrap::AgentFactory::assemble_at(root.clone(), root.join("app-data")).expect("assemble factory"),
        );
        let built = factory.build_agent(Vec::new()).expect("build agent");
        let sessions = crate::interface::session::SessionManager::with_factory(
            built.agent,
            None,
            factory.session_catalog.clone(),
            factory.clone(),
            None,
            Some(built.memory),
            Some(built.skills),
            Some(built.todo),
            built.subagents,
        );
        let catalog = factory.model_catalog.clone();
        App::with_manager(sessions, catalog)
    }

    /// Test App: point `/login`'s persistence target at a unique temp file to avoid polluting the real workspace
    /// (concurrent tests all writing the same file would also truncate each other).
    fn app_with_temp_config(tag: &str) -> App {
        let mut app = app();
        let dir = std::env::temp_dir().join(format!(
            "shirley_login_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        app.set_config_path(dir.join("config.toml"));
        app
    }

    #[test]
    fn session_command_requests_picker_without_sending() {
        let mut app = app();
        for ch in "/session".chars() {
            app.push_input(ch);
        }
        assert!(app.submit().is_none(), "/session should not be sent as a prompt");
        assert!(app.take_session_picker_request(), "should request loading the session list");
        assert!(!app.is_waiting(), "should not enter the waiting-for-reply state");
        assert!(app.input().is_empty(), "the input should be cleared");
    }

    #[test]
    fn session_picker_new_entry_is_first_and_confirms_to_new_session() {
        let mut app = app();
        app.open_session_picker(vec![SessionEntry {
            name: "20240101-000000".into(),
            label: "20240101-000000".into(),
            preview: "old conversation".into(),
            modified_ms: 0,
            turns: 1,
        }]);
        assert!(app.is_session_picker_open());
        // The first item is fixed to the "new" sentinel.
        assert_eq!(app.session_picker().unwrap().entries[0].name, "");
        // No current session -> highlight the new-session entry.
        assert_eq!(app.session_picker().unwrap().selected, 0);
        app.session_picker_cancel();
        assert!(!app.is_session_picker_open());
    }

    #[test]
    fn session_picker_move_does_not_wrap() {
        let mut app = app();
        app.open_session_picker(vec![
            SessionEntry {
                name: "a".into(),
                label: "a".into(),
                preview: String::new(),
                modified_ms: 0,
                turns: 0,
            },
            SessionEntry {
                name: "b".into(),
                label: "b".into(),
                preview: String::new(),
                modified_ms: 0,
                turns: 0,
            },
        ]);
        // 3 items in total (new + a + b).
        app.session_picker_move(-1);
        assert_eq!(
            app.session_picker().unwrap().selected,
            0,
            "should stop at the top, no wrap"
        );
        app.session_picker_move(5);
        assert_eq!(
            app.session_picker().unwrap().selected,
            2,
            "should stop at the bottom, no wrap"
        );
    }

    #[test]
    fn model_command_requests_picker_without_sending() {
        let mut app = app();
        for ch in "/model".chars() {
            app.push_input(ch);
        }
        assert!(app.submit().is_none(), "/model should not be sent as a prompt");
        assert!(app.take_picker_request(), "should request loading the model list");
        assert!(!app.is_waiting(), "should not enter the waiting-for-reply state");
        assert!(app.input().is_empty(), "the input should be cleared");
    }

    #[test]
    fn picker_highlights_current_and_switches_model() {
        let mut app = app();
        let entries = vec![
            ModelEntry::new("a", "model-a", "local"),
            ModelEntry::new("b", "model-b", "local"),
        ];
        app.open_picker(entries);
        assert!(app.is_picker_open());
        // The current model is "test", not in the list -> default highlight item 0.
        assert_eq!(app.picker().unwrap().selected, 0);

        app.picker_move(1);
        assert_eq!(app.picker().unwrap().selected, 1);
        // Wrap-around: moving down again returns to 0.
        app.picker_move(1);
        assert_eq!(app.picker().unwrap().selected, 0);

        app.picker_move(1);
        let chosen = app.picker_confirm();
        assert_eq!(chosen.as_deref(), Some("model-b"));
        assert!(!app.is_picker_open(), "the panel should close after confirm");
        assert_eq!(app.current_model().as_deref(), Some("model-b"));
    }

    /// On `/model` switch, compute a full `ModelConfig` from the model entry and issue it in one shot
    /// （`docs/tool-calling-refactor.md` Step 3）：
    /// (`docs/tool-calling-refactor.md` Step 3):
    #[test]
    fn picker_confirm_applies_model_tool_mode() {
        let mut app = app_with_factory("model_tool_mode");
        app.open_picker(vec![
            ModelEntry::new("plain", "plain", "p"),
            ModelEntry::new("tagged", "tagged", "p").with_tool_mode(Some("content".into())),
        ]);
        app.picker_move(1);
        app.picker_confirm();
        let config = app.agent.as_ref().unwrap().model_config();
        assert!(
            config.tool_calling.is_content(),
            "declaring content should switch to the content protocol"
        );

        // Switch back to an undeclared model -> fall back to the default (Native).
        app.open_picker(vec![ModelEntry::new("plain", "plain", "p")]);
        app.picker_confirm();
        let config = app.agent.as_ref().unwrap().model_config();
        assert!(!config.tool_calling.is_content(), "undeclared should fall back to the default Native");
    }

    #[test]
    fn picker_cancel_keeps_model() {
        let mut app = app();
        let before = app.current_model();
        app.open_picker(vec![ModelEntry::new("x", "x", "p")]);
        app.picker_cancel();
        assert!(!app.is_picker_open());
        assert_eq!(app.current_model(), before, "cancel should not change the model");
    }

    #[test]
    fn picker_highlights_active_model_entry() {
        let mut app = app();
        // Add the current model to the list; it should be highlighted instead of item 0.
        app.open_picker(vec![
            ModelEntry::new("other", "other", "p"),
            ModelEntry::new("test", "test", "p"),
        ]);
        assert_eq!(app.picker().unwrap().selected, 1);
    }

    // ---- Step-by-step login (`/login`) ----

    #[test]
    fn login_command_enters_flow_without_sending() {
        let mut app = app();
        for ch in "/login".chars() {
            app.push_input(ch);
        }
        assert!(app.submit().is_none(), "/login should not be sent as a prompt");
        assert!(app.is_login(), "should enter the login flow");
        assert_eq!(app.login_step(), Some(LoginStep::BaseUrl));
        assert!(!app.is_waiting(), "should not enter the waiting-for-reply state");
        // The input box is pre-filled with the current base_url; Enter keeps it.
        assert_eq!(app.input(), "http://localhost");
    }

    #[test]
    fn login_collects_fields_step_by_step() {
        let mut app = app_with_temp_config("collect");
        app.start_login();

        // Step 1: change base_url.
        app.set_input("http://new-endpoint".into());
        app.submit();
        assert_eq!(app.login_step(), Some(LoginStep::ApiKey));
        assert_eq!(app.input(), "", "api_key defaults to empty");

        // Step 2: fill in the key.
        app.set_input("sk-secret".into());
        app.submit();
        assert_eq!(app.login_step(), Some(LoginStep::Model));
        assert_eq!(app.input(), "test", "the model defaults to the current value");

        // Step 3: change the model -> finish.
        app.set_input("new-model".into());
        app.submit();
        assert!(!app.is_login(), "finishing should exit the login state");
        let agent = app.take_agent().unwrap();
        assert_eq!(agent.model_config().base_url, "http://new-endpoint");
        assert_eq!(agent.model_config().api_key.as_deref(), Some("sk-secret"));
        assert_eq!(agent.model_config().model, "new-model");
    }

    #[test]
    fn login_empty_base_url_is_rejected_and_stays() {
        let mut app = app();
        app.start_login();
        app.set_input("   ".into());
        app.submit();
        // An empty endpoint is meaningless: stay on this step.
        assert_eq!(app.login_step(), Some(LoginStep::BaseUrl));
        assert!(app.is_login());
    }

    #[test]
    fn login_empty_api_key_means_no_auth() {
        let mut app = app_with_temp_config("noauth");
        app.start_login();
        app.set_input("http://x".into());
        app.submit();
        // api_key with a direct Enter -> no auth.
        app.submit();
        app.set_input("m".into());
        app.submit();
        let agent = app.take_agent().unwrap();
        assert_eq!(agent.model_config().api_key, None);
    }

    #[test]
    fn login_cancel_keeps_config_untouched() {
        let mut app = app();
        app.start_login();
        app.set_input("http://changed".into());
        app.cancel_login();
        assert!(!app.is_login());
        assert!(app.input().is_empty(), "cancel should clear the input");
        let agent = app.take_agent().unwrap();
        assert_eq!(
            agent.model_config().base_url,
            "http://localhost",
            "cancel should not change the config"
        );
    }

    #[test]
    fn login_enter_keeps_existing_values() {
        let mut app = app_with_temp_config("keep");
        app.start_login();
        // Entering through all three steps -> keep the original values.
        app.submit(); // base_url kept
        app.submit(); // api_key kept (None -> empty -> None)
        app.submit(); // model kept
        let agent = app.take_agent().unwrap();
        assert_eq!(agent.model_config().base_url, "http://localhost");
        assert_eq!(agent.model_config().model, "test");
    }

    fn agent_with_users(messages: Vec<Message>) -> Agent {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        Agent::builder()
            .model_config(config)
            .messages(messages)
            .build()
            .unwrap()
    }

    #[test]
    fn interrupt_preserves_turn_as_reference() {
        // The Agent already has one user message (simulating run_stream having persisted this turn's User).
        let mut app = App::new(agent_with_users(vec![Message::User {
            content: "hello".into(),
        }]));
        for ch in "hello".chars() {
            app.push_input(ch);
        }
        assert_eq!(app.submit().as_deref(), Some("hello"));
        assert!(app.is_waiting());

        app.request_interrupt();
        assert!(app.interrupt_requested());
        app.complete_turn();

        assert!(!app.is_waiting(), "waiting should end after an interrupt");
        // Requirement 2: an interrupt **keeps** this turn's messages as reference for the next turn, without rolling back or returning to the input box.
        assert!(
            app.input().is_empty(),
            "the input box should be cleared after an interrupt (the turn is kept, not resent)"
        );
        // The UI keeps this turn's user item (items are not truncated).
        assert!(!app.items().is_empty(), "this turn's UI items should be kept after an interrupt");
        assert!(
            app.items().iter().any(|item| matches!(
                item,
                Item::Message(m) if m.role == Role::User
            )),
            "the UI should still contain this turn's user message after an interrupt"
        );
        // The Agent side also keeps this turn's user message (the agent is not truncated / the log is not truncated).
        let agent = app.take_agent().unwrap();
        assert!(
            agent
                .messages()
                .iter()
                .any(|m| matches!(m, Message::User { .. })),
            "the Agent-side user message for this turn should be kept"
        );
    }

    #[test]
    fn rewind_command_fills_last_user_message() {
        let mut app = App::new(agent_with_users(vec![
            Message::User {
                content: "first".into(),
            },
            Message::User {
                content: "second".into(),
            },
        ]));
        // Give the UI two corresponding user items too.
        app.add_message(Message::User {
            content: "first".into(),
        });
        app.add_message(Message::User {
            content: "second".into(),
        });

        app.start_rewind_last_turn();
        // Only the latest one is rolled back: the input box should be filled with "second", and the edit point points at UI item 1.
        assert_eq!(app.input(), "second", "should fill back the last user message");
        assert!(app.is_rewind_edit(), "should enter rewind-edit mode");
        assert_eq!(app.rewind_edit_item(), Some(1), "should point at the message item being edited");
        assert!(app.command_hint().is_some(), "should give a resend hint");
    }

    #[test]
    fn submit_after_rewind_rolls_back_agent_and_ui() {
        let mut app = App::new(agent_with_users(vec![
            Message::User {
                content: "first".into(),
            },
            Message::User {
                content: "second".into(),
            },
        ]));
        app.add_message(Message::User {
            content: "first".into(),
        });
        app.add_message(Message::User {
            content: "second".into(),
        });

        app.start_rewind_last_turn(); // rewind "second"
        // Submit after editing.
        app.push_input('!');
        assert_eq!(app.submit().as_deref(), Some("second!"));

        // The UI is left with the items before the rewind point + this newly submitted one.
        let user_items = app
            .items()
            .iter()
            .filter_map(|i| match i {
                Item::Message(m) if m.role == Role::User => Some(m.content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(user_items, vec!["first".to_owned(), "second!".to_owned()]);
    }

    #[test]
    fn rewind_without_user_message_is_noop() {
        let mut app = App::new(agent_with_users(vec![]));
        app.start_rewind_last_turn();
        assert!(!app.is_rewind_edit(), "no rewindable message should not enter edit mode");
        assert!(app.input().is_empty());
    }

    #[test]
    fn rewind_command_starts_edit_without_sending() {
        let mut app = App::new(agent_with_users(vec![Message::User {
            content: "only".into(),
        }]));
        app.add_message(Message::User {
            content: "only".into(),
        });
        for ch in "/rewind".chars() {
            app.push_input(ch);
        }
        assert!(app.submit().is_none(), "/rewind should not be sent as a prompt");
        assert!(app.is_rewind_edit(), "should enter rewind-edit mode");
        assert!(!app.is_waiting(), "should not enter the waiting-for-reply state");
        assert_eq!(app.input(), "only", "should fill back the last user message");
        assert!(app.command_hint().is_some(), "should give a resend hint");
    }

    #[test]
    fn rewind_edit_enter_resends_and_esc_cancels() {
        let mut app = App::new(agent_with_users(vec![Message::User {
            content: "first".into(),
        }]));
        app.add_message(Message::User {
            content: "first".into(),
        });
        for ch in "/rewind".chars() {
            app.push_input(ch);
        }
        app.submit();
        assert_eq!(app.input(), "first");

        // Esc: cancel this edit -- clear the input, exit edit mode, and do not send.
        app.cancel_rewind_edit();
        assert!(!app.is_rewind_edit(), "cancel should exit edit mode");
        assert!(app.input().is_empty(), "cancel should clear the input");
        assert!(app.command_hint().is_none(), "cancel should clear the hint");

        // Try again, this time editing and pressing Enter to resend.
        app.start_rewind_last_turn();
        app.push_input('!');
        assert!(app.command_hint().is_some(), "edit mode should keep the resend hint");
        assert_eq!(
            app.submit().as_deref(),
            Some("first!"),
            "Enter should resend the edited content"
        );
        assert!(app.is_waiting(), "resending should enter the waiting-for-reply state");
    }

    #[test]
    fn cursor_inserts_and_moves() {
        let mut app = app();
        for ch in "abd".chars() {
            app.push_input(ch);
        }
        app.move_cursor_left();
        app.push_input('c');
        assert_eq!(app.input(), "abcd");
        assert_eq!(app.input_cursor(), 3);

        app.move_cursor_home();
        assert_eq!(app.input_cursor(), 0);
        app.move_cursor_right();
        assert_eq!(app.input_cursor(), 1);
        app.move_cursor_end();
        assert_eq!(app.input_cursor(), 4);
    }

    #[test]
    fn backspace_and_delete_at_cursor() {
        let mut app = app();
        for ch in "abcd".chars() {
            app.push_input(ch);
        }
        app.move_cursor_left();
        app.move_cursor_left();
        app.pop_input(); // delete 'b'
        assert_eq!(app.input(), "acd");
        assert_eq!(app.input_cursor(), 1);
        app.delete_input(); // delete 'c' at the cursor
        assert_eq!(app.input(), "ad");
        assert_eq!(app.input_cursor(), 1);
    }

    #[test]
    fn cursor_handles_multibyte() {
        let mut app = app();
        for ch in "夏莉".chars() {
            app.push_input(ch);
        }
        assert_eq!(app.input_cursor(), "夏莉".len());
        app.move_cursor_left();
        assert_eq!(app.input_cursor(), "夏".len());
        app.pop_input();
        assert_eq!(app.input(), "莉");
    }

    #[test]
    fn history_recall_cycles_and_restores_draft() {
        let mut app = app();
        for prompt in ["first", "second"] {
            for ch in prompt.chars() {
                app.push_input(ch);
            }
            app.submit();
            // Simulate the reply ending, allowing the next submit.
            app.restore_agent(
                Agent::builder()
                    .model_config(
                        ModelConfig::builder()
                            .protocol(ModelProtocol::ChatCompletions)
                            .base_url("http://localhost")
                            .model("test")
                            .build(),
                    )
                    .build()
                    .unwrap(),
            );
        }

        // Type half a draft, then scroll up through history.
        app.push_input('d');
        app.push_input('r');
        app.push_input('a');
        app.push_input('f');
        app.push_input('t');

        app.history_prev();
        assert_eq!(app.input(), "second");
        app.history_prev();
        assert_eq!(app.input(), "first");
        // Scrolling up again at the top still shows the first entry.
        app.history_prev();
        assert_eq!(app.input(), "first");
        app.history_next();
        assert_eq!(app.input(), "second");
        // Scrolling down past the end restores the draft.
        app.history_next();
        assert_eq!(app.input(), "draft");
    }

    #[test]
    fn typing_after_history_exits_recall() {
        let mut app = app();
        for ch in "hello".chars() {
            app.push_input(ch);
        }
        app.submit();
        app.restore_agent(
            Agent::builder()
                .model_config(
                    ModelConfig::builder()
                        .protocol(ModelProtocol::ChatCompletions)
                        .base_url("http://localhost")
                        .model("test")
                        .build(),
                )
                .build()
                .unwrap(),
        );
        app.history_prev();
        assert_eq!(app.input(), "hello");
        app.push_input('!');
        assert_eq!(app.input(), "hello!");
        // After editing, the down key no longer replays history.
        app.history_next();
        assert_eq!(app.input(), "hello!");
    }

    #[test]
    fn scroll_up_from_bottom_starts_at_bottom() {
        let mut app = app();
        app.set_max_scroll(100);
        // While auto-following the bottom, self.scroll is stale at 0; scrolling up should be based on the bottom.
        app.scroll_by(-3);
        assert_eq!(app.scroll(), 97);
        assert!(!app.auto_scroll());
    }

    #[test]
    fn paste_inserts_multiline_without_submitting() {
        let mut app = app();
        app.push_input('a');
        app.insert_input("b\nc\r\nd\r");
        // Both CRLF and CR are normalized to LF.
        assert_eq!(app.input(), "ab\nc\nd\n");
        assert_eq!(app.input_cursor(), "ab\nc\nd\n".len());
        // Pasting should not trigger sending; still in edit mode.
        assert!(!app.is_waiting());
    }

    #[test]
    fn paste_at_cursor_keeps_surrounding_text() {
        let mut app = app();
        for ch in "ac".chars() {
            app.push_input(ch);
        }
        app.move_cursor_left();
        app.insert_input("b");
        assert_eq!(app.input(), "abc");
        assert_eq!(app.input_cursor(), 2);
    }

    #[test]
    fn suggestion_move_clamps_and_selects_highlight() {
        let mut app = app();
        app.push_input('/');
        // Typing `/` lists all commands.
        assert!(app.suggestions().len() >= 3);
        assert_eq!(app.suggestion_index(), 0);

        app.suggestion_move(1);
        assert_eq!(app.suggestion_index(), 1);
        // After reaching the top, no further move up.
        app.suggestion_move(-1);
        app.suggestion_move(-1);
        assert_eq!(app.suggestion_index(), 0);

        // After reaching the bottom, no further move down.
        for _ in 0..10 {
            app.suggestion_move(1);
        }
        assert_eq!(app.suggestion_index(), app.suggestions().len() - 1);
    }

    #[test]
    fn accept_suggestion_uses_highlighted_item() {
        let mut app = app();
        app.push_input('/');
        let names: Vec<String> = app.suggestions().to_vec();
        app.suggestion_move(1);
        let picked = names[1].clone();
        app.accept_suggestion();
        assert_eq!(app.input(), format!("/{picked} "));
        assert!(app.suggestions().is_empty(), "the suggestions should be cleared after accepting");
        assert_eq!(app.suggestion_index(), 0);
    }

    #[test]
    fn editing_input_resets_suggestion_highlight() {
        let mut app = app();
        app.push_input('/');
        app.suggestion_move(1);
        assert_eq!(app.suggestion_index(), 1);
        // Continue typing -> suggestions refresh and the highlight returns to the first item.
        app.push_input('i');
        assert_eq!(app.suggestion_index(), 0);
    }

    #[test]
    fn dismiss_suggestions_keeps_input() {
        let mut app = app();
        app.push_input('/');
        app.push_input('r');
        assert!(!app.suggestions().is_empty());
        app.dismiss_suggestions();
        assert!(app.suggestions().is_empty());
        assert_eq!(app.input(), "/r", "Esc only closes the popup, does not touch the input");
    }
}
