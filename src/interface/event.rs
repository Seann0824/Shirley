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
    // Mouse events: left-drag selection (copy on release) and wheel scrolling.
    Mouse(MouseEvent),
    // Text delivered whole by the terminal via "bracketed paste"; may contain newlines.
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
        // Enable bracketed paste: pasted content arrives as one Paste event, so its newlines
        // are no longer split into individual Enter keys and don't accidentally trigger a send.
        //
        // Also enable mouse capture: **drag selection is implemented by the program itself** (see
        // the `selection` module) — left press records an anchor, dragging extends the highlight,
        // and release writes the selected text to the system clipboard. This makes copying
        // mouse-only, with no need to press a copy key. The cost is that the terminal's native
        // selection is taken over (but the program provides an equivalent, auto-copying one).
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
                    // Forward only meaningful mouse events. `Moved` (movement with no button) is
                    // dropped: the terminal floods these on any mouse motion, and forwarding them all
                    // would make the main loop spin redrawing for nothing.
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
            .unwrap_or_else(|| Err(std::io::Error::other("terminal event stream closed")))
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
