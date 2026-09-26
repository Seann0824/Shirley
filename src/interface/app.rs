use agent_sdk::{Agent, Message};

/// 一条展示用消息：[角色, 内容]，以及它是不是"思考"。
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub thinking: bool,
}

pub struct App {
    agent: Option<Agent>,
    exit: bool,
    input: String,
    messages: Vec<ChatMessage>,
    waiting: bool,
    /// 是否在界面上显示模型的思考过程。
    show_thinking: bool,
}

impl App {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent: Some(agent),
            exit: false,
            input: String::new(),
            messages: Vec::new(),
            waiting: false,
            show_thinking: true,
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

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub fn show_thinking(&self) -> bool {
        self.show_thinking
    }

    pub fn toggle_thinking(&mut self) {
        self.show_thinking = !self.show_thinking;
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
            Message::User { content } => self.messages.push(ChatMessage {
                role: "你".into(),
                content,
                thinking: false,
            }),
            Message::Assistant {
                content,
                reasoning_content,
                ..
            } => {
                // 思考先记下来，让消息顺序保持"先想后答"。
                if let Some(reasoning) = reasoning_content.filter(|r| !r.trim().is_empty()) {
                    self.messages.push(ChatMessage {
                        role: "夏莉".into(),
                        content: reasoning,
                        thinking: true,
                    });
                }
                if let Some(content) = content.filter(|c| !c.is_empty()) {
                    self.messages.push(ChatMessage {
                        role: "夏莉".into(),
                        content,
                        thinking: false,
                    });
                }
            }
            _ => {}
        }
    }

    pub fn add_error(&mut self, error: String) {
        self.messages.push(ChatMessage {
            role: "错误".into(),
            content: error,
            thinking: false,
        });
    }
}
