use agent_sdk::{Agent, AgentEvent, Message};
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{app::App, event::EventHandler, ui, update};
use crate::models::ModelCatalog;

async fn run_agent(
    mut agent: Agent,
    prompt: String,
    updates: mpsc::UnboundedSender<Result<AgentEvent, String>>,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) -> Agent {
    let mut stream = agent.run_stream(&prompt);
    loop {
        tokio::select! {
            biased;
            // 用户打断：优先响应取消信号，drop 掉 stream 让本轮回复立即停止。
            // 助手消息只在 `Finished` 时才写入 agent，因此中断后 agent 干净地停在
            // 本轮用户消息之后，由 `App::complete_turn` 再回退掉这条用户消息。
            _ = &mut cancel => break,
            event = stream.next() => {
                match event {
                    Some(Ok(event)) => {
                        if updates.send(Ok(event)).is_err() {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        let _ = updates.send(Err(error.to_string()));
                        break;
                    }
                    None => break,
                }
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
    /// 向运行中的 agent 任务发送取消信号（用户按 Esc 打断时触发）。
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

impl<'a> Tui<'a> {
    pub fn new(
        terminal: &'a mut ratatui::DefaultTerminal,
        events: EventHandler,
        agent: Agent,
        catalog: Arc<dyn ModelCatalog>,
    ) -> Self {
        Self {
            terminal,
            events,
            app: App::with_catalog(agent, catalog),
            cancel: None,
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
        // 模型列表加载结果独立走一条通道：列表可能来自远端，加载应移出主循环，
        // 期间 UI 不被阻塞（见 `models::ModelCatalog` 的异步接口）。
        let (picker_tx, mut picker_rx) = mpsc::unbounded_channel();
        let mut response: Option<JoinHandle<Agent>> = None;
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        while !self.app.should_exit() {
            self.draw()?;
            tokio::select! {
                biased;
                // 终端事件必须优先于流式更新被消费：AI 回复期间 updates_rx 持续就绪，
                // 若排在按键之前（biased 模式），Esc 等按键会被无限"饿死"，无法打断。
                event = self.events.next() => {
                    if let Some(prompt) = update::update(&mut self.app, event?)
                        && let Some(agent) = self.app.take_agent()
                    {
                        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
                        self.cancel = Some(cancel_tx);
                        response = Some(tokio::spawn(run_agent(
                            agent,
                            prompt,
                            updates_tx.clone(),
                            cancel_rx,
                        )));
                    }
                    // 用户请求打断：向运行中的任务发送取消信号（一次性）。
                    if self.app.interrupt_requested()
                        && let Some(cancel) = self.cancel.take()
                    {
                        let _ = cancel.send(());
                    }
                    // `/model` 触发了模型列表加载：把目录移入后台任务，
                    // 结果经 `picker_tx` 回传后由上面的分支打开选择器。
                    if self.app.take_picker_request() {
                        let catalog = self.app.catalog();
                        let tx = picker_tx.clone();
                        tokio::spawn(async move {
                            let entries = catalog.list().await;
                            let _ = tx.send(entries);
                        });
                    }
                }
                // 模型列表加载完成：打开选择器。
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
                _ = tick.tick(), if response.is_some() => {}
                completed = async { response.as_mut().expect("response exists").await }, if response.is_some() => {
                    while let Ok(update) = updates_rx.try_recv() {
                        self.apply(update);
                    }
                    response = None;
                    self.cancel = None;
                    let agent = completed.map_err(std::io::Error::other)?;
                    self.app.restore_agent(agent);
                    // 收尾：正常完成或被用户打断。打断时回退本轮，把输入退回输入框。
                    self.app.complete_turn();
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

pub async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    agent: Agent,
    catalog: Arc<dyn ModelCatalog>,
) -> std::io::Result<()> {
    let events = EventHandler::new()?;
    Tui::new(terminal, events, agent, catalog).run().await
}
