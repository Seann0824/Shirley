use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, KeyEvent, MouseEvent},
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
    Mouse(MouseEvent),
    Resize(u16, u16),
}

pub struct EventHandler {
    receiver: UnboundedReceiver<std::io::Result<Event>>,
    running: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl EventHandler {
    pub fn new() -> std::io::Result<Self> {
        execute!(std::io::stdout(), EnableMouseCapture)?;
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
                    Ok(event::Event::Mouse(mouse)) => Event::Mouse(mouse),
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
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
}
