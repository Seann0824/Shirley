//! TUI (ratatui) presentation layer.
//!
//! Everything that is specific to the terminal interface lives under this module: the app state
//! machine ([`app`]), the renderer ([`ui`]), key mapping ([`update`]), terminal input plumbing
//! ([`event`]), the markdown renderer ([`markdown`]), mouse selection ([`selection`]) and slash
//! commands ([`command`]). The shared, interface-agnostic orchestration (sessions, project runtime)
//! lives one level up in [`super::session`] / [`super::runtime`]; this module only renders it.

mod app;
mod command;
mod event;
mod markdown;
mod selection;
mod ui;
mod update;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use futures::StreamExt;
use ratatui::{buffer::Buffer, layout::Position};
use shirley_agent_sdk::{Agent, AgentEvent};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use self::app::App;
use self::event::{Event, EventHandler};
use super::session::SessionManager;
use crate::models::ModelCatalog;
use std::sync::Arc;

async fn run_agent(
    mut agent: Agent,
    prompt: String,
    updates: mpsc::UnboundedSender<Result<AgentEvent, String>>,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
    memory: Option<Arc<crate::memory::MemoryRuntime>>,
    status: Arc<crate::status::StateKeeper>,
) -> Agent {
    // Safety net (`docs/agent-status.md` §5.6): if this task is aborted / dropped mid-turn the guard
    // flips the keeper to `Unreachable` + `stale`; on any normal return we disarm it first.
    let mut guard = crate::status::TurnGuard::new(status);
    // Memory V2.5: use this turn's user input as the query, and **prefetch the query vector**
    // (embedding is async HTTP, while `ContextProvider::context()` is synchronous, so it can only
    // be computed ahead of time here). When embedding is unconfigured / prefetch fails it is a no-op --
    // the relevance leg degrades to pure BM25 and never interrupts the conversation.
    let mut cancelled = if let Some(memory) = memory.as_ref() {
        memory.set_query(prompt.as_str());
        tokio::select! {
            biased;
            _ = &mut cancel => true,
            _ = memory.prefetch_query_vector() => false,
        }
    } else {
        false
    };
    let mut stream = agent.run_stream(&prompt);
    let mut failure = None;
    while !cancelled {
        tokio::select! {
            biased;
            // User interrupt: respond to the cancel signal first, drop the stream so this turn's reply stops immediately.
            _ = &mut cancel => cancelled = true,
            event = stream.next() => {
                match event {
                    Some(Ok(event)) => {
                        if updates.send(Ok(event)).is_err() {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        failure = Some(error.to_string());
                        break;
                    }
                    None => break,
                }
            }
        }
    }
    drop(stream);
    for message in agent.finish_interrupted_turn() {
        let _ = updates.send(Ok(AgentEvent::MessageAdded(message)));
    }
    if let Some(error) = failure {
        let _ = updates.send(Err(error));
    }
    guard.disarm();
    agent
}

pub struct Tui<'a> {
    terminal: &'a mut ratatui::DefaultTerminal,
    events: EventHandler,
    app: App,
    /// Sends a cancel signal to the running agent task (triggered when the user presses Esc to interrupt).
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    /// The buffer after the most recent frame render. On mouse release this maps the selection coordinates back to text.
    last_buffer: Option<Buffer>,
    /// System clipboard handle, created lazily on first copy and kept alive afterwards.
    /// Keeping it alive matters because some platforms (X11) need the process to hold the clipboard contents for its whole lifetime.
    clipboard: Option<arboard::Clipboard>,
}

impl<'a> Tui<'a> {
    pub fn new(
        terminal: &'a mut ratatui::DefaultTerminal,
        events: EventHandler,
        sessions: SessionManager,
        model_catalog: Arc<dyn ModelCatalog>,
        needs_login: bool,
    ) -> Self {
        let mut app = App::with_manager(sessions, model_catalog);
        // Automatically enter `/login` when no model service is configured: turn "missing config" from a startup error
        // into an in-TUI guide. The user can press Esc to cancel at any time (and can still run `/login` manually afterwards).
        if needs_login {
            app.start_login();
        }
        Self {
            terminal,
            events,
            app,
            cancel: None,
            last_buffer: None,
            clipboard: None,
        }
    }

    pub fn draw(&mut self) -> std::io::Result<()> {
        // Split the borrows; both terminal and app need &mut.
        let app = &mut self.app;
        let completed = self.terminal.draw(|frame| {
            ui::draw(frame, app);
            // Selection highlight is drawn on top of everything: overlay inverse video directly on buffer cells, flushed each frame.
            if let Some(selection) = app.selection() {
                selection.highlight(frame.buffer_mut());
            }
        })?;
        // Only keep this frame's buffer when a selection exists (for extracting text on left-button release): cloning the whole buffer
        // is not cheap, so there is no need to do it every frame. During a drag we hit this every frame, so a snapshot always exists on release.
        if app.selection().is_some() {
            self.last_buffer = Some(completed.buffer.clone());
        }
        Ok(())
    }

    /// Handle mouse events: left-button drag = select and auto-copy; scroll wheel = scroll.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.app.begin_selection(position),
            MouseEventKind::Drag(MouseButton::Left) => self.app.extend_selection(position),
            MouseEventKind::Up(MouseButton::Left) => self.finish_selection(),
            MouseEventKind::ScrollUp => self.app.scroll_lines(-3),
            MouseEventKind::ScrollDown => self.app.scroll_lines(3),
            _ => {}
        }
    }

    /// Left-button release: extract the selected text and write it to the system clipboard (a plain click does not copy).
    fn finish_selection(&mut self) {
        let Some(selection) = self.app.take_selection() else {
            return;
        };
        if selection.is_click() {
            self.app.toggle_compact_at(selection.anchor);
            return;
        }
        let Some(buffer) = self.last_buffer.as_ref() else {
            return;
        };
        let text = selection.text(buffer);
        if text.is_empty() {
            return;
        }
        self.copy_to_clipboard(&text);
    }

    /// Write to the system clipboard. On failure, tell the user via a system message; never swallow it silently.
    fn copy_to_clipboard(&mut self, text: &str) {
        let result = match self.clipboard.as_mut() {
            Some(clipboard) => clipboard.set_text(text.to_owned()),
            None => match arboard::Clipboard::new() {
                Ok(mut clipboard) => {
                    let result = clipboard.set_text(text.to_owned());
                    self.clipboard = Some(clipboard);
                    result
                }
                Err(error) => Err(error),
            },
        };
        if let Err(error) = result {
            self.app
                .add_system_message(format!("failed to copy to clipboard: {error}"));
        }
    }

    pub fn exit(&mut self) {
        self.app.exit();
    }

    /// Accumulate one `AgentEvent` into the foreground session.
    ///
    /// The accumulation logic is centralized in `Session::apply_event` (the same event-handling path shared with desktop) --
    /// the TUI only forwards, no longer matching events itself.
    fn apply(&mut self, update: Result<AgentEvent, String>) {
        self.app.apply_event(update);
    }

    pub async fn run(&mut self) -> std::io::Result<()> {
        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
        // The model-list load result travels on its own channel: the list may come from a remote source, so loading should move off the main loop
        // and the UI must not be blocked meanwhile (see the async interface of `models::ModelCatalog`).
        let (picker_tx, mut picker_rx) = mpsc::unbounded_channel();
        let mut response: Option<JoinHandle<Agent>> = None;
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        while !self.app.should_exit() {
            self.draw()?;
            tokio::select! {
                biased;
                // Terminal events must be consumed before streaming updates: during an AI reply updates_rx is continuously ready,
                // and if it is queued ahead of key presses (biased mode), keys like Esc would be "starved" forever and could not interrupt.
                event = self.events.next() => {
                    // Mouse events do not go through `update`: selection needs the latest frame's buffer and the system clipboard,
                    // which only `Tui` holds, so handle them directly here.
                    match event? {
                        Event::Mouse(mouse) => self.handle_mouse(mouse),
                        event => {
                            if let Some(prompt) = update::update(&mut self.app, event)
                                && let Some(agent) = self.app.take_agent()
                            {
                                // Memory: use this turn's user input as the query (including query-vector prefetch,
                                // see `run_agent`), so the provider can do relevance retrieval when assembling the request
                                // and inject it (core.md resident + top-k relevant, V2.5 hybrid retrieval).
                                let memory = self.app.memory.clone();
                                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
                                self.cancel = Some(cancel_tx);
                                let status = self.app.status.clone();
                                response = Some(tokio::spawn(run_agent(
                                    agent,
                                    prompt,
                                    updates_tx.clone(),
                                    cancel_rx,
                                    memory,
                                    status,
                                )));
                            }
                            // User requested interrupt: send a cancel signal to the running task (one-shot).
                            if self.app.interrupt_requested()
                                && let Some(cancel) = self.cancel.take()
                            {
                                let _ = cancel.send(());
                            }
                            // `/model` triggered a model-list load: move the catalog into a background task,
                            // and after the result comes back via `picker_tx` the branch above opens the picker.
                            if self.app.take_picker_request() {
                                let catalog = self.app.catalog();
                                let tx = picker_tx.clone();
                                tokio::spawn(async move {
                                    let entries = catalog.list().await;
                                    let _ = tx.send(entries);
                                });
                            }
                            // `/session` triggered a session-list load: the session catalog is a local scan (synchronous),
                            // so just fetch the list in the main loop and open the picker; no background task needed.
                            if self.app.take_session_picker_request() {
                                let catalog = self.app.session_catalog();
                                match catalog.list() {
                                    Ok(entries) => self.app.open_session_picker(entries),
                                    Err(error) => self
                                        .app
                                        .add_system_message(format!("failed to load session list: {error}")),
                                }
                            }
                        }
                    }
                }
                // Model-list load finished: open the picker.
                Some(entries) = picker_rx.recv() => {
                    self.app.open_picker(entries);
                }
                Some(update) = updates_rx.recv(), if response.is_some() => {
                    let mut next = update;
                    loop {
                        let show_compression = matches!(next, Ok(AgentEvent::CompressionStarted));
                        self.apply(next);
                        if show_compression { break; }
                        match updates_rx.try_recv() {
                            Ok(update) => next = update,
                            Err(_) => break,
                        }
                    }
                },
                // Heartbeat: re-evaluate time-based transitions (slow tool / stalled) ~1 Hz.
                // This is the only thing that ticks when no AgentEvent arrives -- that is the whole
                // point of stall detection (a silent turn must still be observable).
                _ = tick.tick(), if response.is_some() => {
                    self.app.status.tick();
                }
                completed = async { response.as_mut().expect("response exists").await }, if response.is_some() => {
                    while let Ok(update) = updates_rx.try_recv() {
                        self.apply(update);
                    }
                    response = None;
                    self.cancel = None;
                    let agent = completed.map_err(std::io::Error::other)?;
                    self.app.restore_agent(agent);
                    // Wrap up: keep this turn's content and clear the streaming state.
                    self.app.complete_turn();
                }
            }
        }
        if let Some(task) = response {
            if let Some(cancel) = self.cancel.take() {
                let _ = cancel.send(());
            }
            let agent = task.await.map_err(std::io::Error::other)?;
            while let Ok(update) = updates_rx.try_recv() {
                self.apply(update);
            }
            self.app.restore_agent(agent);
            self.app.complete_turn();
        }
        // Before exit, run one memory curation on the foreground session (`docs/memory.md` §5.1): for a single session chatted to the end
        // and then exited directly, the switch path never happens, so this is the only moment that can trigger curation for "this turn ended".
        // best-effort, and it **must be awaited** -- the runtime shuts down right after the main loop ends, so a spawned task
        // might be dropped before it finishes.
        self.app.finish().await;
        self.exit();
        Ok(())
    }
}

pub async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    sessions: SessionManager,
    model_catalog: Arc<dyn ModelCatalog>,
    needs_login: bool,
) -> std::io::Result<()> {
    let events = EventHandler::new()?;
    Tui::new(terminal, events, sessions, model_catalog, needs_login)
        .run()
        .await
}
