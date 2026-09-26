use std::{future::Future, pin::Pin};

use agent_sdk::{Agent, AgentEvent, Message};
use futures::StreamExt;
use tokio::sync::mpsc;

use super::{app::App, event::EventHandler, ui, update};

enum AgentUpdate {
    Message(Message),
    Error(String),
}

async fn run_agent(
    mut agent: Agent,
    prompt: String,
    updates: mpsc::UnboundedSender<AgentUpdate>,
) -> Agent {
    let mut stream = agent.run_stream(&prompt);
    while let Some(event) = stream.next().await {
        match event {
            Ok(AgentEvent::MessageAdded(message)) => {
                if updates.send(AgentUpdate::Message(message)).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(error) => {
                let _ = updates.send(AgentUpdate::Error(error.to_string()));
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

    pub async fn run(&mut self) -> std::io::Result<()> {
        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
        let mut response: Option<Pin<Box<dyn Future<Output = Agent>>>> = None;
        while !self.app.should_exit() {
            self.draw()?;
            tokio::select! {
                event = self.events.next() => {
                    if let Some(prompt) = update::update(&mut self.app, event?) {
                        if let Some(agent) = self.app.take_agent() {
                            response = Some(Box::pin(run_agent(agent, prompt, updates_tx.clone())));
                        }
                    }
                }
                Some(update) = updates_rx.recv(), if response.is_some() => match update {
                    AgentUpdate::Message(message) => self.app.add_message(message),
                    AgentUpdate::Error(error) => self.app.add_error(error),
                },
                agent = async { response.as_mut().expect("response exists").await }, if response.is_some() => {
                    while let Ok(update) = updates_rx.try_recv() {
                        match update {
                            AgentUpdate::Message(message) => self.app.add_message(message),
                            AgentUpdate::Error(error) => self.app.add_error(error),
                        }
                    }
                    response = None;
                    self.app.restore_agent(agent);
                }
            }
        }
        self.exit();
        Ok(())
    }
}

pub async fn run(terminal: &mut ratatui::DefaultTerminal, agent: Agent) -> std::io::Result<()> {
    let events = EventHandler::new()?;
    Tui::new(terminal, events, agent).run().await
}
