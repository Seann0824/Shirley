use agent_sdk::{Agent, Message, Usage};
use std::time::Instant;

use super::ui::MessageCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Summary,
    Error,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub thinking: bool,
}

#[derive(Debug, Clone)]
pub struct ToolCallView {
    pub name: String,
    // 原始 JSON 参数，留着是为了在 TUI 里展示读了哪个文件
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ToolGroup {
    pub calls: Vec<ToolCallView>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Message(ChatMessage),
    Tools(ToolGroup),
}

pub struct App {
    agent: Option<Agent>,
    exit: bool,
    input: String,
    items: Vec<Item>,
    waiting: bool,
    waiting_since: Option<Instant>,
    show_thinking: bool,
    scroll: usize,
    auto_scroll: bool,
    // 渲染层每帧回写，输入层拿不到布局所以存这儿
    max_scroll: usize,
    last_usage: Option<Usage>,
    total_usage: Usage,
    compressing: bool,
    context_usage: Option<(u64, u64)>,
    pub(crate) message_cache: Option<MessageCache>,
}

impl App {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent: Some(agent),
            exit: false,
            input: String::new(),
            items: Vec::new(),
            waiting: false,
            waiting_since: None,
            show_thinking: true,
            scroll: 0,
            auto_scroll: true,
            max_scroll: 0,
            last_usage: None,
            total_usage: Usage::default(),
            compressing: false,
            context_usage: None,
            message_cache: None,
        }
    }

    pub fn last_usage(&self) -> Option<&Usage> {
        self.last_usage.as_ref()
    }

    pub fn total_usage(&self) -> &Usage {
        &self.total_usage
    }

    pub fn record_usage(&mut self, usage: Usage) {
        self.total_usage = self.total_usage + usage;
        self.last_usage = Some(usage);
    }

    pub fn start_compression(&mut self) {
        self.compressing = true;
    }

    pub fn finish_compression(&mut self) {
        self.compressing = false;
        self.context_usage = None;
    }

    pub fn is_compressing(&self) -> bool {
        self.compressing
    }

    pub fn record_context_usage(&mut self, used_tokens: u64, limit_tokens: u64) {
        self.context_usage = Some((used_tokens, limit_tokens));
    }

    pub fn context_usage(&self) -> Option<(u64, u64)> {
        self.context_usage
    }

    pub fn max_scroll(&self) -> usize {
        self.max_scroll
    }

    pub fn set_max_scroll(&mut self, max_scroll: usize) {
        // 上限缩小时旧位置可能越界，夹回来免得停在空白
        self.max_scroll = max_scroll;
        if !self.auto_scroll && self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn auto_scroll(&self) -> bool {
        self.auto_scroll
    }

    // 负为向上。上滚即退出跟随底部
    pub fn scroll_by(&mut self, delta: i32) {
        let max = self.max_scroll;
        let clamped = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            self.scroll.saturating_add(delta as usize).min(max)
        };
        self.scroll = clamped;
        self.auto_scroll = clamped >= max;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.auto_scroll = true;
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

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub fn waiting_seconds(&self) -> Option<u64> {
        self.waiting_since.map(|since| since.elapsed().as_secs())
    }

    pub fn show_thinking(&self) -> bool {
        self.show_thinking
    }

    pub fn toggle_thinking(&mut self) {
        self.show_thinking = !self.show_thinking;
        self.message_cache = None;
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
        self.waiting_since = Some(Instant::now());
        let prompt = std::mem::take(&mut self.input);
        self.push_message(Role::User, prompt.clone(), false);
        Some(prompt)
    }

    pub fn take_agent(&mut self) -> Option<Agent> {
        self.agent.take()
    }

    pub fn restore_agent(&mut self, agent: Agent) {
        self.agent = Some(agent);
        self.waiting = false;
        self.waiting_since = None;
    }

    fn push_message(&mut self, role: Role, content: String, thinking: bool) {
        self.items.push(Item::Message(ChatMessage {
            role,
            content,
            thinking,
        }));
        self.message_cache = None;
    }

    pub fn start_tool_calls(&mut self, calls: Vec<ToolCallView>) {
        if calls.is_empty() {
            return;
        }
        self.items.push(Item::Tools(ToolGroup { calls }));
        self.message_cache = None;
    }

    pub fn add_message(&mut self, message: Message) {
        match message {
            Message::User { content } => self.push_message(Role::User, content, false),
            Message::Assistant {
                content,
                reasoning_content,
                tool_calls,
            } => {
                // 思考先记下来，让消息顺序保持"先想后答"。
                if let Some(reasoning) = reasoning_content.filter(|r| !r.trim().is_empty()) {
                    self.push_message(Role::Assistant, reasoning, true);
                }
                if let Some(content) = content.filter(|c| !c.is_empty()) {
                    self.push_message(Role::Assistant, content, false);
                }
                if !tool_calls.is_empty() {
                    let calls = tool_calls
                        .into_iter()
                        .map(|call| ToolCallView {
                            name: call.name,
                            arguments: call.arguments,
                        })
                        .collect();
                    self.start_tool_calls(calls);
                }
            }
            Message::ContextSummary { content } => self.push_message(Role::Summary, content, false),
            Message::Tool { .. } | Message::System { .. } => {}
        }
    }

    pub fn add_error(&mut self, error: String) {
        self.compressing = false;
        self.push_message(Role::Error, error, false);
    }
}
