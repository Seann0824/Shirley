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
    input_cursor: usize,
    // 已发送的提示词历史，供上/下键回放。
    history: Vec<String>,
    // None 表示不在历史浏览中；Some(i) 表示当前显示的是 history[i]。
    history_index: Option<usize>,
    // 进入历史浏览前暂存的未发送输入，用于向下回到末尾时恢复。
    history_draft: String,
    items: Vec<Item>,
    waiting: bool,
    waiting_since: Option<Instant>,
    show_thinking: bool,
    // 是否展开工具调用的完整参数（默认收起，只显示一行摘要）。
    show_tool_args: bool,
    scroll: usize,
    auto_scroll: bool,
    // 渲染层每帧回写，输入层拿不到布局所以存这儿
    max_scroll: usize,
    last_usage: Option<Usage>,
    total_usage: Usage,
    compressing: bool,
    context_usage: Option<(u64, u64)>,
    streaming_delta_start: Option<usize>,
    pub(crate) message_cache: Option<MessageCache>,
}

impl App {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent: Some(agent),
            exit: false,
            input: String::new(),
            input_cursor: 0,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            items: Vec::new(),
            waiting: false,
            waiting_since: None,
            show_thinking: true,
            show_tool_args: false,
            scroll: 0,
            auto_scroll: true,
            max_scroll: 0,
            last_usage: None,
            total_usage: Usage::default(),
            compressing: false,
            context_usage: None,
            streaming_delta_start: None,
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
        // 跟随底部时 self.scroll 是陈旧的（draw 每帧只在本地算有效位置），
        // 以底部为基准，否则第一次上滚会从旧位置跳走。
        let base = if self.auto_scroll { max } else { self.scroll };
        let clamped = if delta < 0 {
            base.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            base.saturating_add(delta as usize).min(max)
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

    /// 输入光标的字节偏移，始终落在字符边界上。
    pub fn input_cursor(&self) -> usize {
        self.input_cursor
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

    pub fn show_tool_args(&self) -> bool {
        self.show_tool_args
    }

    pub fn toggle_tool_args(&mut self) {
        self.show_tool_args = !self.show_tool_args;
        self.message_cache = None;
    }

    pub fn push_input(&mut self, ch: char) {
        // 一旦开始编辑就退出历史浏览，避免回放与草稿互相打架。
        self.history_index = None;
        self.input.insert(self.input_cursor, ch);
        self.input_cursor += ch.len_utf8();
    }

    /// 粘贴：把整段文本按字面插入光标处。
    /// 换行统一成 `\n` 作为普通字符写入，不会触发发送。
    pub fn insert_input(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.history_index = None;
        // 终端可能送 CRLF，统一成 LF，避免输入框里混入 `\r`。
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        self.input.insert_str(self.input_cursor, &normalized);
        self.input_cursor += normalized.len();
    }

    /// 退格：删除光标前一个字符。
    pub fn pop_input(&mut self) {
        if let Some((index, _)) = self.input[..self.input_cursor].char_indices().last() {
            self.input.remove(index);
            self.input_cursor = index;
        }
    }

    /// Delete：删除光标处字符。
    pub fn delete_input(&mut self) {
        if self.input_cursor < self.input.len() {
            self.input.remove(self.input_cursor);
        }
    }

    pub fn move_cursor_left(&mut self) {
        if let Some((index, _)) = self.input[..self.input_cursor].char_indices().last() {
            self.input_cursor = index;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if let Some(ch) = self.input[self.input_cursor..].chars().next() {
            self.input_cursor += ch.len_utf8();
        }
    }

    pub fn move_cursor_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.input_cursor = self.input.len();
    }

    pub fn submit(&mut self) -> Option<String> {
        if self.waiting || self.agent.is_none() || self.input.trim().is_empty() {
            return None;
        }
        self.waiting = true;
        self.waiting_since = Some(Instant::now());
        let prompt = std::mem::take(&mut self.input);
        self.input_cursor = 0;
        self.history.push(prompt.clone());
        self.history_index = None;
        self.history_draft.clear();
        self.push_message(Role::User, prompt.clone(), false);
        Some(prompt)
    }

    /// 上键：回放到更早的一条历史。首次进入时暂存当前输入。
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(0) => 0,
            Some(index) => index - 1,
            None => {
                self.history_draft = self.input.clone();
                self.history.len() - 1
            }
        };
        self.history_index = Some(index);
        self.set_input(self.history[index].clone());
    }

    /// 下键：回放到更晚的一条历史，走到末尾则恢复进入浏览前的输入。
    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.set_input(self.history[index + 1].clone());
        } else {
            self.history_index = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.set_input(draft);
        }
    }

    /// 整体替换输入内容，并把光标放到末尾。
    fn set_input(&mut self, text: String) {
        self.input = text;
        self.input_cursor = self.input.len();
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

    pub fn append_streaming_delta(&mut self, delta: String, thinking: bool) {
        if delta.is_empty() {
            return;
        }

        if self.streaming_delta_start.is_some() {
            if let Some(Item::Message(message)) = self.items.last_mut() {
                if message.role == Role::Assistant && message.thinking == thinking {
                    message.content.push_str(&delta);
                    self.message_cache = None;
                    return;
                }
            }
        } else {
            self.streaming_delta_start = Some(self.items.len());
        }

        self.push_message(Role::Assistant, delta, thinking);
    }

    pub fn finish_streaming_deltas(&mut self) {
        if let Some(start) = self.streaming_delta_start.take() {
            self.items.truncate(start);
            self.message_cache = None;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_sdk::{ModelConfig, ModelProtocol};

    fn app() -> App {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        App::new(Agent::builder().model_config(config).build())
    }

    #[test]
    fn cursor_inserts_and_moves() {
        let mut app = app();
        for ch in "abd".chars() {
            app.push_input(ch);
        }
        app.move_cursor_left();
        app.push_input('c');
        assert_eq!(app.input(), "abcd");
        assert_eq!(app.input_cursor(), 3);

        app.move_cursor_home();
        assert_eq!(app.input_cursor(), 0);
        app.move_cursor_right();
        assert_eq!(app.input_cursor(), 1);
        app.move_cursor_end();
        assert_eq!(app.input_cursor(), 4);
    }

    #[test]
    fn backspace_and_delete_at_cursor() {
        let mut app = app();
        for ch in "abcd".chars() {
            app.push_input(ch);
        }
        app.move_cursor_left();
        app.move_cursor_left();
        app.pop_input(); // 删除 'b'
        assert_eq!(app.input(), "acd");
        assert_eq!(app.input_cursor(), 1);
        app.delete_input(); // 删除光标处 'c'
        assert_eq!(app.input(), "ad");
        assert_eq!(app.input_cursor(), 1);
    }

    #[test]
    fn cursor_handles_multibyte() {
        let mut app = app();
        for ch in "夏莉".chars() {
            app.push_input(ch);
        }
        assert_eq!(app.input_cursor(), "夏莉".len());
        app.move_cursor_left();
        assert_eq!(app.input_cursor(), "夏".len());
        app.pop_input();
        assert_eq!(app.input(), "莉");
    }

    #[test]
    fn history_recall_cycles_and_restores_draft() {
        let mut app = app();
        for prompt in ["first", "second"] {
            for ch in prompt.chars() {
                app.push_input(ch);
            }
            app.submit();
            // 模拟回复结束，允许下一次提交。
            app.restore_agent(
                Agent::builder()
                    .model_config(
                        ModelConfig::builder()
                            .protocol(ModelProtocol::ChatCompletions)
                            .base_url("http://localhost")
                            .model("test")
                            .build(),
                    )
                    .build(),
            );
        }

        // 输入一半的草稿，再上翻历史。
        app.push_input('d');
        app.push_input('r');
        app.push_input('a');
        app.push_input('f');
        app.push_input('t');

        app.history_prev();
        assert_eq!(app.input(), "second");
        app.history_prev();
        assert_eq!(app.input(), "first");
        // 到头再上翻仍是第一条。
        app.history_prev();
        assert_eq!(app.input(), "first");
        app.history_next();
        assert_eq!(app.input(), "second");
        // 下翻越过末尾恢复草稿。
        app.history_next();
        assert_eq!(app.input(), "draft");
    }

    #[test]
    fn typing_after_history_exits_recall() {
        let mut app = app();
        for ch in "hello".chars() {
            app.push_input(ch);
        }
        app.submit();
        app.restore_agent(
            Agent::builder()
                .model_config(
                    ModelConfig::builder()
                        .protocol(ModelProtocol::ChatCompletions)
                        .base_url("http://localhost")
                        .model("test")
                        .build(),
                )
                .build(),
        );
        app.history_prev();
        assert_eq!(app.input(), "hello");
        app.push_input('!');
        assert_eq!(app.input(), "hello!");
        // 编辑后下键不再回放历史。
        app.history_next();
        assert_eq!(app.input(), "hello!");
    }

    #[test]
    fn scroll_up_from_bottom_starts_at_bottom() {
        let mut app = app();
        app.set_max_scroll(100);
        // 自动跟随底部时 self.scroll 陈旧为 0，上滚应以底部为基准。
        app.scroll_by(-3);
        assert_eq!(app.scroll(), 97);
        assert!(!app.auto_scroll());
    }

    #[test]
    fn paste_inserts_multiline_without_submitting() {
        let mut app = app();
        app.push_input('a');
        app.insert_input("b\nc\r\nd\r");
        // CRLF 与 CR 都归一为 LF。
        assert_eq!(app.input(), "ab\nc\nd\n");
        assert_eq!(app.input_cursor(), "ab\nc\nd\n".len());
        // 粘贴不应触发发送，仍处于编辑态。
        assert!(!app.is_waiting());
    }

    #[test]
    fn paste_at_cursor_keeps_surrounding_text() {
        let mut app = app();
        for ch in "ac".chars() {
            app.push_input(ch);
        }
        app.move_cursor_left();
        app.insert_input("b");
        assert_eq!(app.input(), "abc");
        assert_eq!(app.input_cursor(), 2);
    }
}
