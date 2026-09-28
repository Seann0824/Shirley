use agent_sdk::{Agent, AgentEvent, Message};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{app::App, event::EventHandler, ui, update};

async fn run_agent(
    mut agent: Agent,
    prompt: String,
    updates: mpsc::UnboundedSender<Result<AgentEvent, String>>,
) -> Agent {
    let mut stream = agent.run_stream(&prompt);
    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => {
                if updates.send(Ok(event)).is_err() {
                    break;
                }
            }
            Err(error) => {
                let _ = updates.send(Err(error.to_string()));
                break;
            }
        }
    }
    drop(stream);
    agent
}

pub struct Tui<'a> {
    terminal: &'a mut ratatui::DefaultTerminal,
    events: EventHandler,
    app: App,
}

impl<'a> Tui<'a> {
    pub fn new(
        terminal: &'a mut ratatui::DefaultTerminal,
        events: EventHandler,
        agent: Agent,
    ) -> Self {
        Self {
            terminal,
            events,
            app: App::new(agent),
        }
    }

    pub fn draw(&mut self) -> std::io::Result<()> {
        // 拆开借用，terminal 和 app 都要 &mut
        let app = &mut self.app;
        self.terminal.draw(|frame| ui::draw(frame, app))?;
        Ok(())
    }

    pub fn exit(&mut self) {
        self.app.exit();
    }

    fn apply(&mut self, update: Result<AgentEvent, String>) {
        match update {
            Ok(AgentEvent::MessageAdded(Message::User { .. })) => {}
            Ok(AgentEvent::MessageAdded(message)) => {
                if matches!(&message, Message::Assistant { .. }) {
                    self.app.finish_streaming_deltas();
                }
                self.app.add_message(message);
            }
            Ok(AgentEvent::ContentDelta(delta)) => self.app.append_streaming_delta(delta, false),
            Ok(AgentEvent::ReasoningDelta(delta)) => self.app.append_streaming_delta(delta, true),
            Ok(AgentEvent::Usage(usage)) => self.app.record_usage(usage),
            Ok(AgentEvent::CompressionStarted) => self.app.start_compression(),
            Ok(AgentEvent::CompressionFinished) => self.app.finish_compression(),
            Ok(AgentEvent::ContextUsage {
                used_tokens,
                limit_tokens,
            }) => self.app.record_context_usage(used_tokens, limit_tokens),
            Err(error) => self.app.add_error(error),
            // Tool and run-finished events do not need a separate TUI update.
            Ok(AgentEvent::ToolStarted { .. })
            | Ok(AgentEvent::ToolFinished { .. })
            | Ok(AgentEvent::Finished(_)) => {}
        }
    }

    pub async fn run(&mut self) -> std::io::Result<()> {
        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
        let mut response: Option<JoinHandle<Agent>> = None;
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        while !self.app.should_exit() {
            self.draw()?;
            tokio::select! {
                biased;
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
                event = self.events.next() => {
                    if let Some(prompt) = update::update(&mut self.app, event?) {
                        if let Some(agent) = self.app.take_agent() {
                            response = Some(tokio::spawn(run_agent(agent, prompt, updates_tx.clone())));
                        }
                    }
                }
                _ = tick.tick(), if response.is_some() => {}
                completed = async { response.as_mut().expect("response exists").await }, if response.is_some() => {
                    while let Ok(update) = updates_rx.try_recv() {
                        self.apply(update);
                    }
                    response = None;
                    let agent = completed.map_err(std::io::Error::other)?;
                    self.app.restore_agent(agent);
                }
            }
        }
        if let Some(task) = response {
            task.abort();
        }
        self.exit();
        Ok(())
    }
}

pub async fn run(terminal: &mut ratatui::DefaultTerminal, agent: Agent) -> std::io::Result<()> {
    let events = EventHandler::new()?;
    Tui::new(terminal, events, agent).run().await
}
