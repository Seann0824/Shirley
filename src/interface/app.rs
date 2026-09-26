use agent_sdk::{Agent, Message};

pub struct App {
    agent: Option<Agent>,
    exit: bool,
    input: String,
    messages: Vec<(String, String)>,
    waiting: bool,
}

impl App {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent: Some(agent),
            exit: false,
            input: String::new(),
            messages: Vec::new(),
            waiting: false,
        }
    }

    pub fn should_exit(&self) -> bool {
        self.exit
    }

    pub fn exit(&mut self) {
        self.exit = true;
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn messages(&self) -> &[(String, String)] {
        &self.messages
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub fn push_input(&mut self, ch: char) {
        self.input.push(ch);
    }

    pub fn pop_input(&mut self) {
        self.input.pop();
    }

    pub fn submit(&mut self) -> Option<String> {
        if self.waiting || self.agent.is_none() || self.input.trim().is_empty() {
            return None;
        }
        self.waiting = true;
        Some(std::mem::take(&mut self.input))
    }

    pub fn take_agent(&mut self) -> Option<Agent> {
        self.agent.take()
    }

    pub fn restore_agent(&mut self, agent: Agent) {
        self.agent = Some(agent);
        self.waiting = false;
    }

    pub fn add_message(&mut self, message: Message) {
        match message {
            Message::User { content } => self.messages.push(("你".into(), content)),
            Message::Assistant {
                content: Some(content),
                ..
            } if !content.is_empty() => {
                self.messages.push(("AI".into(), content));
            }
            _ => {}
        }
    }

    pub fn add_error(&mut self, error: String) {
        self.messages.push(("错误".into(), error));
    }
}
