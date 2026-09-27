use agent_sdk::{Agent, AgentEvent, Message, Usage};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{app::App, event::EventHandler, ui, update};

enum AgentUpdate {
    Message(Message),
    Usage(Usage),
    CompressionStarted,
    CompressionFinished,
    ContextUsage { used_tokens: u64, limit_tokens: u64 },
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
            Ok(AgentEvent::CompressionStarted) => {
                if updates.send(AgentUpdate::CompressionStarted).is_err() {
                    break;
                }
            }
            Ok(AgentEvent::CompressionFinished) => {
                if updates.send(AgentUpdate::CompressionFinished).is_err() {
                    break;
                }
            }
            Ok(AgentEvent::ContextUsage {
                used_tokens,
                limit_tokens,
            }) => {
                if updates
                    .send(AgentUpdate::ContextUsage {
                        used_tokens,
                        limit_tokens,
                    })
                    .is_err()
                {
                    break;
                }
            }
            // 提交时已本地回显，避免同一条用户消息重复显示。
            Ok(AgentEvent::MessageAdded(Message::User { .. })) => {}
            Ok(AgentEvent::MessageAdded(message)) => {
                if updates.send(AgentUpdate::Message(message)).is_err() {
                    break;
                }
            }
            Ok(AgentEvent::Usage(usage)) => {
                if updates.send(AgentUpdate::Usage(usage)).is_err() {
                    break;
                }
            }
            // 工具的展示与结果都由带 tool_calls 的 Assistant 消息
            // 和后续的 Tool 消息驱动，这里忽略开始/结束事件即可。
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

    fn apply(&mut self, update: AgentUpdate) {
        match update {
            AgentUpdate::Message(message) => self.app.add_message(message),
            AgentUpdate::Usage(usage) => self.app.record_usage(usage),
            AgentUpdate::CompressionStarted => self.app.start_compression(),
            AgentUpdate::CompressionFinished => self.app.finish_compression(),
            AgentUpdate::ContextUsage {
                used_tokens,
                limit_tokens,
            } => self.app.record_context_usage(used_tokens, limit_tokens),
            AgentUpdate::Error(error) => self.app.add_error(error),
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
                        let show_compression = matches!(next, AgentUpdate::CompressionStarted);
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
