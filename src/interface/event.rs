use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, KeyEvent},
    execute,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

pub enum Event {
    Key(KeyEvent),
    // 终端以「括号粘贴」形式整体送来的文本，内部可能含换行。
    Paste(String),
    Resize(u16, u16),
}

pub struct EventHandler {
    receiver: UnboundedReceiver<std::io::Result<Event>>,
    running: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl EventHandler {
    pub fn new() -> std::io::Result<Self> {
        // 打开括号粘贴：粘贴内容会被终端包成一个 Paste 事件整体送达，
        // 其中的换行不再被拆成一个个 Enter 键，从而避免误触发送。
        //
        // **不开启鼠标捕获**：捕获会把"拖动选择文本"从终端手里抢走，
        // 导致用户无法用终端原生的方式选中 / 复制（这是"TUI 里复制不了文本"的
        // 根因）。滚轮滚动因此改由键盘 `PageUp` / `PageDown` 承担。
        execute!(std::io::stdout(), EnableBracketedPaste)?;
        let (sender, receiver) = mpsc::unbounded_channel();
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = Arc::clone(&running);
        let worker = thread::spawn(move || {
            while worker_running.load(Ordering::Relaxed) {
                match event::poll(Duration::from_millis(50)) {
                    Ok(false) => continue,
                    Ok(true) => {}
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
                let next = match event::read() {
                    Ok(event::Event::Key(key)) => Event::Key(key),
                    Ok(event::Event::Paste(text)) => Event::Paste(text),
                    Ok(event::Event::Resize(width, height)) => Event::Resize(width, height),
                    Ok(_) => continue,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                };
                if sender.send(Ok(next)).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            receiver,
            running,
            worker: Some(worker),
        })
    }

    pub async fn next(&mut self) -> std::io::Result<Event> {
        self.receiver
            .recv()
            .await
            .unwrap_or_else(|| Err(std::io::Error::other("终端事件流已关闭")))
    }
}

impl Drop for EventHandler {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    }
}
