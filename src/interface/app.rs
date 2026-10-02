use agent_sdk::{Agent, Message, Usage};
use std::sync::Arc;
use std::time::Instant;

use crate::models::{ModelCatalog, ModelEntry, StaticCatalog};

use super::command::{CommandManager, CommandOutcome, DEFAULT_PREFIX};

use super::ui::MessageCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Summary,
    System,
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

/// `/model` 选择器的状态。列表已在打开时由模型目录加载完毕，
/// 这里只保存展示所需的数据与高亮位置。
pub struct ModelPicker {
    pub entries: Vec<ModelEntry>,
    /// 当前高亮的条目下标。
    pub selected: usize,
    /// 正在使用的模型值（用于在列表里打标），打开时快照一次。
    pub current: Option<String>,
}

/// `/rewind` 历史回溯面板：列出 Agent 仍记得的用户消息，选中后回退重发。
pub struct RewindPicker {
    /// 候选项：最近的在最上。
    pub entries: Vec<RewindEntry>,
    /// 当前高亮下标。
    pub selected: usize,
}

/// 一条可回溯的用户消息。同时记录它在 Agent 消息表与 UI 条目表里的位置，
/// 回退时两边一起截断，保证视图与真实上下文对齐。
#[derive(Debug, Clone)]
pub struct RewindEntry {
    /// 该用户消息在 `agent.messages` 里的下标（回退到此下标之前）。
    pub agent_index: usize,
    /// 对应的 UI `items` 下标（回退到此下标之前）。
    pub ui_index: usize,
    /// 消息原文，选中后填入输入框供编辑。
    pub content: String,
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
    /// 快捷指令解析器。只作用于 TUI，不参与任何发送给 AI 的上下文。
    commands: CommandManager,
    /// `/` 触发的模糊指令候选（如 `/inti` → `init`）。空表示当前无候选。
    suggestions: Vec<String>,
    /// 候选中当前高亮项的下标（↑↓ / ←→ 移动，Tab / Enter 采纳）。
    suggestion_index: usize,
    /// 模型目录：`/model` 的模型来源。接口稳定，实现可换（静态 / 远端）。
    ///
    /// 用 `Arc` 持有，便于把加载任务移到后台（远端列表不应阻塞 UI）。
    catalog: Arc<dyn ModelCatalog>,
    /// 打开中的模型选择器；`None` 表示未打开。
    picker: Option<ModelPicker>,
    /// `/model` 被触发、但模型列表尚未加载完成时置位。
    /// 由 TUI 主循环取走并异步加载目录（列表可能来自远端）。
    picker_requested: bool,
    /// 本轮回复开始时 UI `items` 的长度，打断时据此回退界面。
    ui_turn_start: Option<usize>,
    /// 本轮提交的输入，打断后退回输入框供修改重发。
    turn_prompt: Option<String>,
    /// 用户按 Esc 请求打断当前回复；由主循环读取后触发取消（不清除，待完成时消费）。
    interrupt_requested: bool,
    /// `/rewind` 打开的历史回溯面板；`None` 表示未打开。
    rewind: Option<RewindPicker>,
    /// 已确认的回溯点 `(agent 消息下标, UI 条目下标)`：下次提交前先回退到这里。
    rewind_target: Option<(usize, usize)>,
    /// 待滚动到的 UI 条目下标：回溯确认后置位，渲染层据此把该消息滚入视野。
    /// 渲染层取走即清空（一次性）。
    /// 输入框提示语覆盖：指令打开的编辑态（如回溯编辑）用它说明"Enter 重发 / Esc 取消"。
    /// 为空时用默认提示。任何普通输入都会清掉它。
    command_hint: Option<String>,
}

impl App {
    /// 用内置静态模型目录构造（默认）。
    pub fn new(agent: Agent) -> Self {
        Self::with_catalog(agent, Arc::new(StaticCatalog::builtin()))
    }

    /// 注入模型目录构造。远端目录将来从这里传入，UI 无需改动。
    pub fn with_catalog(agent: Agent, catalog: Arc<dyn ModelCatalog>) -> Self {
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
            commands: CommandManager::new(DEFAULT_PREFIX),
            suggestions: Vec::new(),
            suggestion_index: 0,
            catalog,
            picker: None,
            picker_requested: false,
            ui_turn_start: None,
            turn_prompt: None,
            interrupt_requested: false,
            rewind: None,
            rewind_target: None,
            command_hint: None,
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
        self.on_input_changed();
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
        self.on_input_changed();
    }

    /// 退格：删除光标前一个字符。
    pub fn pop_input(&mut self) {
        if let Some((index, _)) = self.input[..self.input_cursor].char_indices().last() {
            self.input.remove(index);
            self.input_cursor = index;
            self.on_input_changed();
        }
    }

    /// Delete：删除光标处字符。
    pub fn delete_input(&mut self) {
        if self.input_cursor < self.input.len() {
            self.input.remove(self.input_cursor);
            self.on_input_changed();
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
        // 快捷指令：以 prefix 开头且命中内置指令时，先解析。
        // - 展开成 prompt：把展开后内容替换回 input，UI 显示的是展开后的自然语言，
        //   AI 收到的也只是 prompt，与直接打字无任何差别。
        // - 本地系统消息：不打扰 AI，直接提示用户。
        match self.commands.resolve(&self.input) {
            CommandOutcome::Prompt(expanded) => {
                self.input = expanded;
            }
            CommandOutcome::SystemMessage(message) => {
                self.input.clear();
                self.input_cursor = 0;
                self.add_system_message(message);
                self.suggestions.clear();
                return None;
            }
            CommandOutcome::ModelPicker => {
                // 不发送、不落历史：清空输入并请求主循环异步加载模型列表。
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.command_hint = Some(
                    " ↑↓ 选择 · Enter 确认 · Esc 取消 ".to_owned(),
                );
                self.picker_requested = true;
                return None;
            }
            CommandOutcome::RewindPicker => {
                // 不发送、不落历史：清空输入并打开回溯面板。候选来自 Agent 当前消息表，
                // 无需异步加载（与 `/model` 的差别正在于此）。
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.open_rewind();
                // 无可回溯消息时 open_rewind 会给系统提示，此时别留下指令提示。
                self.command_hint = self
                    .is_rewind_open()
                    .then(|| " ↑↓ 选择 · Enter 编辑重发 · Esc 取消 ".to_owned());
                return None;
            }
            CommandOutcome::Unknown => {}
        }

        // 若存在已确认的回溯点（`/rewind` 选择后未提交），先同步回退 Agent 与界面，
        // 再作为全新一轮发送——保证 UI 视图与真实上下文始终一致。
        if let Some((agent_index, ui_index)) = self.rewind_target.take() {
            if let Some(agent) = self.agent.as_mut() {
                agent.rewind(agent_index);
            }
            self.items.truncate(ui_index);
            self.streaming_delta_start = None;
            self.message_cache = None;
        }

        self.command_hint = None;
        self.waiting = true;
        self.waiting_since = Some(Instant::now());
        let prompt = std::mem::take(&mut self.input);
        self.input_cursor = 0;
        self.history.push(prompt.clone());
        self.history_index = None;
        self.history_draft.clear();
        // 记录本轮起点：打断时据此把界面与上下文一起回退。
        self.ui_turn_start = Some(self.items.len());
        self.turn_prompt = Some(prompt.clone());
        self.interrupt_requested = false;
        self.push_message(Role::User, prompt.clone(), false);
        self.suggestions.clear();
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

    /// 本轮回复结束（正常完成或被用户打断）后的收尾。
    ///
    /// 若本轮被用户打断，把 Agent 与界面一起回退到本轮用户消息之前，
    /// 并把该条输入退回输入框，供用户修改后重发；会话状态不丢。
    pub fn complete_turn(&mut self) {
        self.waiting = false;
        self.waiting_since = None;

        if self.interrupt_requested {
            self.interrupt_requested = false;
            // Agent 侧：丢掉本轮用户消息及其后可能已完成的 assistant/tool 链。
            if let Some(prompt) = self
                .agent
                .as_mut()
                .and_then(|agent| agent.rewind_last_user_turn())
            {
                self.turn_prompt = Some(prompt);
            }
            // 界面侧：回退到本轮开始前（清掉半截流式回复）。
            if let Some(start) = self.ui_turn_start.take() {
                self.items.truncate(start);
            }
            self.streaming_delta_start = None;
            self.message_cache = None;
            // 把本轮输入退回输入框。
            if let Some(prompt) = self.turn_prompt.take() {
                self.set_input(prompt);
            }
        }

        self.ui_turn_start = None;
        self.turn_prompt = None;
    }

    // ---- 打断当前回复 ----

    /// 用户按 Esc 请求打断当前回复（仅在等待回复时有效）。
    pub fn request_interrupt(&mut self) {
        if self.waiting {
            self.interrupt_requested = true;
        }
    }

    /// 是否已请求打断（主循环据此向运行中的任务发送取消信号）。
    pub fn interrupt_requested(&self) -> bool {
        self.interrupt_requested
    }

    // ---- 历史回溯（`/rewind`）----

    /// 打开回溯面板：列出 Agent 仍记得的用户消息（最近的在最上）。
    ///
    /// 被压缩掉的历史不在 `agent.messages` 里，因此天然不可回溯——压缩边界
    /// 之后才谈得上回溯，语义上也自洽。
    pub fn open_rewind(&mut self) {
        let Some(agent) = self.agent.as_ref() else {
            return;
        };
        // Agent 侧的用户消息（下标 + 原文）。
        let agent_users: Vec<(usize, String)> = agent
            .messages()
            .iter()
            .enumerate()
            .filter_map(|(i, m)| match m {
                Message::User { content } => Some((i, content.clone())),
                _ => None,
            })
            .collect();
        if agent_users.is_empty() {
            self.add_system_message("没有可回溯的消息。".to_owned());
            return;
        }
        // UI 侧的用户条目下标：与 Agent 用户消息一一对应（每次提交各产生一条）。
        let ui_users: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| match item {
                Item::Message(m) if m.role == Role::User => Some(i),
                _ => None,
            })
            .collect();

        // 从两端对齐：最近的一条在面板最上方。
        let entries: Vec<RewindEntry> = agent_users
            .iter()
            .rev()
            .zip(ui_users.iter().rev())
            .map(|((agent_index, content), ui_index)| RewindEntry {
                agent_index: *agent_index,
                ui_index: *ui_index,
                content: content.clone(),
            })
            .collect();

        self.rewind = Some(RewindPicker { entries, selected: 0 });
        self.message_cache = None;
    }

    pub fn rewind(&self) -> Option<&RewindPicker> {
        self.rewind.as_ref()
    }

    pub fn is_rewind_open(&self) -> bool {
        self.rewind.is_some()
    }

    pub fn rewind_move(&mut self, delta: i32) {
        let Some(picker) = self.rewind.as_mut() else {
            return;
        };
        let len = picker.entries.len() as i32;
        if len == 0 {
            return;
        }
        picker.selected = (picker.selected as i32 + delta).rem_euclid(len) as usize;
    }

    /// 确认回溯：把选中消息填回输入框并记录回溯点，等待用户编辑后提交。
    pub fn rewind_confirm(&mut self) {
        let Some(picker) = self.rewind.take() else {
            return;
        };
        self.message_cache = None;
        let Some(entry) = picker.entries.get(picker.selected).cloned() else {
            return;
        };
        self.rewind_target = Some((entry.agent_index, entry.ui_index));
        self.set_input(entry.content);
        // 进入"编辑重发"态：Enter 重新发送，Esc 取消本次改动。
        // 输入框由渲染层就地画到该消息的位置（见 `rewind_edit_item`），无需额外滚动。
        self.command_hint = Some(" Enter 重新发送 · Esc 取消本次修改 ".to_owned());
    }

    pub fn rewind_cancel(&mut self) {
        if self.rewind.take().is_some() {
            self.message_cache = None;
        }
        self.command_hint = None;
    }

    /// 指令编辑态的输入框提示覆盖（无则为 `None`）。
    pub fn command_hint(&self) -> Option<&str> {
        self.command_hint.as_deref()
    }

    /// 是否处于"回溯编辑重发"态（已确认回溯点、等待编辑后提交）。
    pub fn is_rewind_edit(&self) -> bool {
        self.rewind_target.is_some()
    }

    /// 回溯编辑态下正在被编辑的 UI 条目下标（渲染层据此把该消息就地画成输入框）。
    pub fn rewind_edit_item(&self) -> Option<usize> {
        self.rewind_target.map(|(_, ui_index)| ui_index)
    }

    /// 仅测试用：直接进入回溯编辑态并指向某条 UI 条目。
    #[cfg(test)]
    pub fn set_rewind_edit_for_test(&mut self, ui_index: usize) {
        self.rewind_target = Some((0, ui_index));
    }

    /// 取消本次回溯编辑：清掉回溯点与输入，Agent / 界面均不改动。
    /// 用户"不回车直接取消"的落点。
    pub fn cancel_rewind_edit(&mut self) {
        if self.rewind_target.take().is_some() {
            self.input.clear();
            self.input_cursor = 0;
            self.command_hint = None;
            self.suggestions.clear();
        }
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

    /// TUI 本地系统提示（如指令反馈），不进入发送给 AI 的上下文。
    pub fn add_system_message(&mut self, content: String) {
        self.push_message(Role::System, content, false);
    }

    /// 根据当前输入刷新模糊指令候选。仅在输入以 prefix 开头且未精确命中时给出。
    /// 无候选时清空。每次编辑输入后调用。
    pub fn refresh_suggestions(&mut self) {
        self.suggestions = self.commands.fuzzy_match(&self.input).into_iter().map(|(n, _)| n).collect();
        // 候选集变了，高亮回到第一项。
        self.suggestion_index = 0;
    }

    /// 当前指令候选（按匹配度排序）。
    pub fn suggestions(&self) -> &[String] {
        &self.suggestions
    }

    /// 当前高亮的候选下标（渲染层据此画 `▶`）。
    pub fn suggestion_index(&self) -> usize {
        self.suggestion_index
    }

    /// 移动候选高亮（`delta` 为 -1 / +1）。夹在 `[0, len-1]`，到边界不绕回。
    /// 无候选时不动。
    pub fn suggestion_move(&mut self, delta: isize) {
        if self.suggestions.is_empty() {
            return;
        }
        let last = self.suggestions.len() - 1;
        let next = self.suggestion_index as isize + delta;
        self.suggestion_index = next.clamp(0, last as isize) as usize;
    }

    /// 放弃当前候选浮层（Esc）：只清候选，不动输入本身。
    pub fn dismiss_suggestions(&mut self) {
        self.suggestions.clear();
        self.suggestion_index = 0;
    }

    /// Tab / Enter 补全：采纳**当前高亮**的候选，把输入替换为 `/<name> ` 并清空候选，
    /// 继续编辑参数。
    pub fn accept_suggestion(&mut self) {
        let index = self.suggestion_index.min(self.suggestions.len().saturating_sub(1));
        if let Some(name) = self.suggestions.get(index).cloned() {
            self.input = format!("{}{} ", DEFAULT_PREFIX, name);
            self.input_cursor = self.input.len();
            self.suggestions.clear();
            self.suggestion_index = 0;
        }
    }

    // ---- 模型选择器（`/model`）----

    /// 主循环取走"需要加载模型列表"的请求。
    pub fn take_picker_request(&mut self) -> bool {
        std::mem::take(&mut self.picker_requested)
    }

    /// 模型目录句柄（供主循环移入后台任务异步加载）。接口稳定，实现可换。
    pub fn catalog(&self) -> Arc<dyn ModelCatalog> {
        Arc::clone(&self.catalog)
    }

    /// 用加载好的模型列表打开选择器，高亮当前正在使用的模型。
    pub fn open_picker(&mut self, entries: Vec<ModelEntry>) {
        if entries.is_empty() {
            self.command_hint = None;
            self.add_system_message("没有可用的模型。".to_owned());
            return;
        }
        let current = self.current_model();
        let selected = current
            .as_ref()
            .and_then(|value| entries.iter().position(|e| &e.value == value))
            .unwrap_or(0);
        self.picker = Some(ModelPicker {
            entries,
            selected,
            current,
        });
        self.message_cache = None;
    }

    /// 当前模型值（来自 Agent 配置）。
    pub fn current_model(&self) -> Option<String> {
        self.agent.as_ref().map(|agent| agent.model_config().model.clone())
    }

    pub fn picker(&self) -> Option<&ModelPicker> {
        self.picker.as_ref()
    }

    /// 选择器上下移动高亮。
    pub fn picker_move(&mut self, delta: i32) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let len = picker.entries.len() as i32;
        if len == 0 {
            return;
        }
        let next = (picker.selected as i32 + delta).rem_euclid(len);
        picker.selected = next as usize;
    }

    /// 确认选择：切换 Agent 的模型并关闭面板。返回被选中的模型值。
    pub fn picker_confirm(&mut self) -> Option<String> {
        let picker = self.picker.take()?;
        self.message_cache = None;
        self.command_hint = None;
        let entry = picker.entries.get(picker.selected).cloned()?;
        if let Some(agent) = self.agent.as_mut() {
            agent.set_model(entry.value.clone());
        }
        self.add_system_message(format!("已切换模型：{}", entry.label));
        Some(entry.value)
    }

    /// 取消选择。
    pub fn picker_cancel(&mut self) {
        if self.picker.take().is_some() {
            self.message_cache = None;
        }
        self.command_hint = None;
    }

    /// 选择器是否打开（主循环据此把按键导向面板而非输入框）。
    pub fn is_picker_open(&self) -> bool {
        self.picker.is_some()
    }

    /// 输入变化时重算候选（在 push/insert/pop/delete/move 等之后统一调用）。
    pub fn on_input_changed(&mut self) {
        // 回溯编辑态下保留"Enter 重发 / Esc 取消"提示——用户改动的是待重发内容，
        // 该态仍然成立；其余情况一旦改动输入即恢复默认提示。
        if !self.is_rewind_edit() {
            self.command_hint = None;
        }
        self.refresh_suggestions();
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
    fn model_command_requests_picker_without_sending() {
        let mut app = app();
        for ch in "/model".chars() {
            app.push_input(ch);
        }
        assert!(app.submit().is_none(), "/model 不应作为 prompt 发送");
        assert!(app.take_picker_request(), "应请求加载模型列表");
        assert!(!app.is_waiting(), "不应进入等待回复状态");
        assert!(app.input().is_empty(), "输入应被清空");
    }

    #[test]
    fn picker_highlights_current_and_switches_model() {
        let mut app = app();
        let entries = vec![
            ModelEntry::new("a", "model-a", "local"),
            ModelEntry::new("b", "model-b", "local"),
        ];
        app.open_picker(entries);
        assert!(app.is_picker_open());
        // 当前模型是 "test"，不在列表里 → 默认高亮第 0 项。
        assert_eq!(app.picker().unwrap().selected, 0);

        app.picker_move(1);
        assert_eq!(app.picker().unwrap().selected, 1);
        // 环回：再下移回到 0。
        app.picker_move(1);
        assert_eq!(app.picker().unwrap().selected, 0);

        app.picker_move(1);
        let chosen = app.picker_confirm();
        assert_eq!(chosen.as_deref(), Some("model-b"));
        assert!(!app.is_picker_open(), "确认后应关闭面板");
        assert_eq!(app.current_model().as_deref(), Some("model-b"));
    }

    #[test]
    fn picker_cancel_keeps_model() {
        let mut app = app();
        let before = app.current_model();
        app.open_picker(vec![ModelEntry::new("x", "x", "p")]);
        app.picker_cancel();
        assert!(!app.is_picker_open());
        assert_eq!(app.current_model(), before, "取消不应改变模型");
    }

    #[test]
    fn picker_highlights_active_model_entry() {
        let mut app = app();
        // 把当前模型加进列表，应高亮它而不是第 0 项。
        app.open_picker(vec![
            ModelEntry::new("other", "other", "p"),
            ModelEntry::new("test", "test", "p"),
        ]);
        assert_eq!(app.picker().unwrap().selected, 1);
    }

    fn agent_with_users(messages: Vec<Message>) -> Agent {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        Agent::builder().model_config(config).messages(messages).build()
    }

    #[test]
    fn interrupt_reverts_turn_and_restores_input() {
        // Agent 已有一条用户消息（模拟 run_stream 已把本轮 User 落库）。
        let mut app = App::new(agent_with_users(vec![Message::User {
            content: "hello".into(),
        }]));
        for ch in "hello".chars() {
            app.push_input(ch);
        }
        assert_eq!(app.submit().as_deref(), Some("hello"));
        assert!(app.is_waiting());

        app.request_interrupt();
        assert!(app.interrupt_requested());
        app.complete_turn();

        assert!(!app.is_waiting(), "打断后应结束等待");
        assert_eq!(app.input(), "hello", "本轮输入应退回输入框");
        // 界面回退到本轮之前：不应残留本轮用户条目。
        assert!(app.items().is_empty(), "打断后应清掉本轮的界面条目");
        // Agent 侧本轮用户消息也被回退。
        let agent = app.take_agent().unwrap();
        assert!(
            agent.messages().iter().all(|m| !matches!(m, Message::User { .. })),
            "Agent 侧本轮用户消息应被回退"
        );
    }

    #[test]
    fn rewind_confirm_fills_input_and_records_target() {
        let mut app = App::new(agent_with_users(vec![
            Message::User { content: "first".into() },
            Message::User { content: "second".into() },
        ]));
        // 让 UI 也有对应的两条用户条目。
        app.add_message(Message::User { content: "first".into() });
        app.add_message(Message::User { content: "second".into() });

        app.open_rewind();
        assert!(app.is_rewind_open());
        // 最近的在最上。
        assert_eq!(app.rewind().unwrap().entries[0].content, "second");
        assert_eq!(app.rewind().unwrap().entries[1].content, "first");

        app.rewind_move(1);
        assert_eq!(app.rewind().unwrap().selected, 1);
        app.rewind_confirm();
        assert!(!app.is_rewind_open(), "确认后应关闭面板");
        assert_eq!(app.input(), "first", "选中消息应填入输入框");
        // 确认后应进入编辑重发态，并把输入框就地定位到该消息位置。
        assert!(app.is_rewind_edit(), "应进入回溯编辑态");
        assert_eq!(app.rewind_edit_item(), Some(0), "应定位到被编辑的消息条目");
    }

    #[test]
    fn submit_after_rewind_rolls_back_agent_and_ui() {
        let mut app = App::new(agent_with_users(vec![
            Message::User { content: "first".into() },
            Message::User { content: "second".into() },
        ]));
        app.add_message(Message::User { content: "first".into() });
        app.add_message(Message::User { content: "second".into() });

        app.open_rewind();
        app.rewind_move(1); // 选 "first"
        app.rewind_confirm();
        // 编辑后提交。
        app.push_input('!');
        assert_eq!(app.submit().as_deref(), Some("first!"));

        // 界面只剩回退点之前的条目 + 新提交的这条。
        let user_items = app
            .items()
            .iter()
            .filter_map(|i| match i {
                Item::Message(m) if m.role == Role::User => Some(m.content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(user_items, vec!["first!".to_owned()]);
    }

    #[test]
    fn rewind_cancel_keeps_state() {
        let mut app = App::new(agent_with_users(vec![Message::User {
            content: "only".into(),
        }]));
        app.add_message(Message::User { content: "only".into() });
        app.open_rewind();
        app.rewind_cancel();
        assert!(!app.is_rewind_open());
        assert!(app.input().is_empty(), "取消不应改动输入");
        assert!(!app.is_rewind_edit(), "取消后面板关闭且未进入编辑态");
    }

    #[test]
    fn rewind_command_opens_picker_without_sending() {
        let mut app = App::new(agent_with_users(vec![Message::User {
            content: "only".into(),
        }]));
        app.add_message(Message::User { content: "only".into() });
        for ch in "/rewind".chars() {
            app.push_input(ch);
        }
        assert!(app.submit().is_none(), "/rewind 不应作为 prompt 发送");
        assert!(app.is_rewind_open(), "应打开回溯面板");
        assert!(!app.is_waiting(), "不应进入等待回复状态");
        assert!(app.input().is_empty(), "输入应被清空");
        assert!(app.command_hint().is_some(), "应给出面板操作提示");
    }

    #[test]
    fn rewind_edit_enter_resends_and_esc_cancels() {
        let mut app = App::new(agent_with_users(vec![Message::User {
            content: "first".into(),
        }]));
        app.add_message(Message::User { content: "first".into() });
        for ch in "/rewind".chars() {
            app.push_input(ch);
        }
        app.submit();
        app.rewind_confirm();
        assert_eq!(app.input(), "first");

        // Esc：取消本次改动——清输入、退出编辑态，且不发送。
        app.cancel_rewind_edit();
        assert!(!app.is_rewind_edit(), "取消后应退出编辑态");
        assert!(app.input().is_empty(), "取消应清空输入");
        assert!(app.command_hint().is_none(), "取消应清掉提示");

        // 再来一次，这次编辑后回车重发。
        app.open_rewind();
        app.rewind_confirm();
        app.push_input('!');
        assert!(app.command_hint().is_some(), "编辑态应保留重发提示");
        assert_eq!(app.submit().as_deref(), Some("first!"), "回车应重发编辑后的内容");
        assert!(app.is_waiting(), "重发应进入等待回复状态");
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

    #[test]
    fn suggestion_move_clamps_and_selects_highlight() {
        let mut app = app();
        app.push_input('/');
        // 输入 `/` 会列出全部指令。
        assert!(app.suggestions().len() >= 3);
        assert_eq!(app.suggestion_index(), 0);

        app.suggestion_move(1);
        assert_eq!(app.suggestion_index(), 1);
        // 到顶后不再上移。
        app.suggestion_move(-1);
        app.suggestion_move(-1);
        assert_eq!(app.suggestion_index(), 0);

        // 到底后不再下移。
        for _ in 0..10 {
            app.suggestion_move(1);
        }
        assert_eq!(app.suggestion_index(), app.suggestions().len() - 1);
    }

    #[test]
    fn accept_suggestion_uses_highlighted_item() {
        let mut app = app();
        app.push_input('/');
        let names: Vec<String> = app.suggestions().to_vec();
        app.suggestion_move(1);
        let picked = names[1].clone();
        app.accept_suggestion();
        assert_eq!(app.input(), format!("/{picked} "));
        assert!(app.suggestions().is_empty(), "采纳后应清空候选");
        assert_eq!(app.suggestion_index(), 0);
    }

    #[test]
    fn editing_input_resets_suggestion_highlight() {
        let mut app = app();
        app.push_input('/');
        app.suggestion_move(1);
        assert_eq!(app.suggestion_index(), 1);
        // 继续打字 → 候选刷新，高亮回到第一项。
        app.push_input('i');
        assert_eq!(app.suggestion_index(), 0);
    }

    #[test]
    fn dismiss_suggestions_keeps_input() {
        let mut app = app();
        app.push_input('/');
        app.push_input('r');
        assert!(!app.suggestions().is_empty());
        app.dismiss_suggestions();
        assert!(app.suggestions().is_empty());
        assert_eq!(app.input(), "/r", "Esc 只关浮层，不动输入");
    }
}
