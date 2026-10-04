use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
        EnableMouseCapture, KeyEvent, MouseEvent, MouseEventKind,
    },
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
    // 鼠标事件：用于左键拖动选中（松开即复制）与滚轮滚动。
    Mouse(MouseEvent),
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
        // 同时打开鼠标捕获：**拖动选中由程序自己实现**（见 `selection` 模块）——
        // 左键按下记锚点、拖动延伸高亮、松开即把选中文本写入系统剪贴板。
        // 这样复制全程只用鼠标，不需要再按一次复制键。代价是终端原生选择被接管
        // （但程序已提供等效能力，且是自动复制的）。
        execute!(
            std::io::stdout(),
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
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
                    // 只转发有意义的鼠标事件。`Moved`（无按键的移动）会被丢弃：
                    // 鼠标只要移动终端就会狂发这类事件，全量转发会让主循环空转重绘。
                    Ok(event::Event::Mouse(mouse)) => match mouse.kind {
                        MouseEventKind::Moved => continue,
                        _ => Event::Mouse(mouse),
                    },
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
        let _ = execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
    }
}
