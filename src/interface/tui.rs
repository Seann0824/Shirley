use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use futures::StreamExt;
use ratatui::{buffer::Buffer, layout::Position};
use shirley_agent_sdk::{Agent, AgentEvent};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::session::SessionManager;
use super::{app::App, event::Event, event::EventHandler, ui, update};
use crate::models::ModelCatalog;
use std::sync::Arc;

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
    /// 最近一帧渲染后的缓冲区。鼠标松开时据此把选区坐标映射回文本。
    last_buffer: Option<Buffer>,
    /// 系统剪贴板句柄，首次复制时惰性创建并常驻。
    /// 常驻的意义在于：某些平台（X11）需要进程存活期间一直持有剪贴板内容。
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
        // 未配置模型服务时自动进入 `/login`：把"缺配置"从启动错误变成 TUI 内
        // 的一次引导。用户可随时 Esc 取消（取消后仍可手动 `/login`）。
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
        // 拆开借用，terminal 和 app 都要 &mut。
        let app = &mut self.app;
        let completed = self.terminal.draw(|frame| {
            ui::draw(frame, app);
            // 选区高亮画在一切之上：直接给缓冲区单元格叠反色，随帧刷出。
            if let Some(selection) = app.selection() {
                selection.highlight(frame.buffer_mut());
            }
        })?;
        // 仅在存在选区时留存本帧缓冲区（供松开左键时提取文本）：克隆整块缓冲区
        // 不便宜，没必要每帧都做。拖动过程中每帧都会走到这里，故松开时必有快照。
        if app.selection().is_some() {
            self.last_buffer = Some(completed.buffer.clone());
        }
        Ok(())
    }

    /// 处理鼠标事件：左键拖动 = 选中并自动复制；滚轮 = 滚动。
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

    /// 松开左键：提取选中文本并写入系统剪贴板（原地单击不复制）。
    fn finish_selection(&mut self) {
        let Some(selection) = self.app.take_selection() else {
            return;
        };
        if selection.is_click() {
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

    /// 写入系统剪贴板。失败时以系统消息告知，不静默吞掉。
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
            self.app.add_system_message(format!("复制到剪贴板失败：{error}"));
        }
    }

    pub fn exit(&mut self) {
        self.app.exit();
    }

    /// 把一条 `AgentEvent` 累加到前台会话。
    ///
    /// 累加逻辑收敛在 `Session::apply_event`（与 desktop 共享同一条事件处理路径）——
    /// TUI 只做转发，不再自己 match 事件。
    fn apply(&mut self, update: Result<AgentEvent, String>) {
        self.app.apply_event(update);
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
                    // 鼠标事件不走 `update`：选区需要最近一帧的缓冲区与系统剪贴板，
                    // 这两样只有 `Tui` 持有，故在这里直接处理。
                    match event? {
                        Event::Mouse(mouse) => self.handle_mouse(mouse),
                        event => {
                            if let Some(prompt) = update::update(&mut self.app, event)
                                && let Some(agent) = self.app.take_agent()
                            {
                                // 记忆：把本轮用户输入作为 query，供 provider 在
                                // 组装请求时做相关检索注入（core.md 常驻 + top-k 相关）。
                                if let Some(memory) = self.app.memory.as_ref() {
                                    memory.set_query(prompt.as_str());
                                }
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
                            // `/session` 触发了会话列表加载：会话目录是本地扫描（同步），
                            // 直接在主循环取列表并打开选择器即可，无需后台任务。
                            if self.app.take_session_picker_request() {
                                let catalog = self.app.session_catalog();
                                match catalog.list() {
                                    Ok(entries) => self.app.open_session_picker(entries),
                                    Err(error) => self
                                        .app
                                        .add_system_message(format!("加载会话列表失败：{error}")),
                                }
                            }
                        }
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
    sessions: SessionManager,
    model_catalog: Arc<dyn ModelCatalog>,
    needs_login: bool,
) -> std::io::Result<()> {
    let events = EventHandler::new()?;
    Tui::new(terminal, events, sessions, model_catalog, needs_login)
        .run()
        .await
}
