use ratatui::layout::Position;
use shirley_agent_sdk::{Agent, AgentEvent, Message, Usage};
use std::sync::Arc;
use std::time::Instant;

use crate::models::{ModelCatalog, ModelEntry, StaticCatalog};
use crate::session::{EmptySessionCatalog, SessionCatalog, SessionEntry};

use super::command::{CommandManager, CommandOutcome, DEFAULT_PREFIX};
use super::session::SessionManager;

use super::selection::Selection;
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

/// `/session` 选择器的状态。列表已在打开时由会话目录加载完毕，
/// 这里只保存展示所需的数据与高亮位置。
///
/// 列表首项固定是「＋ 新建会话」哨兵（`name` 为空串），其余是真实会话；
/// 确认时按 `name` 是否为空分流到"新建"或"切换"。
pub struct SessionPicker {
    pub entries: Vec<SessionEntry>,
    /// 当前高亮的条目下标。
    pub selected: usize,
    /// 当前会话名（用于在列表里打标），打开时快照一次。
    pub current: Option<String>,
}

impl SessionPicker {
    /// 「新建会话」哨兵条目的名字（空串即代表新建入口）。
    pub const NEW_NAME: &'static str = "";
}

/// `/login` 分步流程的当前阶段。
///
/// 顺序固定：先确认端点（base_url），再确认密钥（api_key），最后确认模型。
/// 端点在前是因为没有它后两者无从谈起；模型放最后，因为改完端点后
/// 通常想顺手换个默认模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStep {
    BaseUrl,
    ApiKey,
    Model,
}

impl LoginStep {
    /// 该步在提示里的问法。
    fn prompt(&self) -> &'static str {
        match self {
            LoginStep::BaseUrl => "请输入模型服务地址 base_url",
            LoginStep::ApiKey => "请输入 API Key（本地服务可留空，直接回车跳过）",
            LoginStep::Model => "请输入默认模型名",
        }
    }
}

/// `/login` 分步流程的进行态：已收集的字段 + 当前阶段。
///
/// 只活在内存里，`Esc` 或走完即清空——没走完不落盘，避免写进半截配置。
pub struct LoginFlow {
    pub step: LoginStep,
    /// 已确认的 base_url（第一步之后必有）。
    pub base_url: String,
    /// 已确认的 api_key（`None` 表示无鉴权）。
    pub api_key: Option<String>,
}

pub struct App {
    /// 多会话编排器：每会话一个独立 `Agent`，`active` 指向前台。
    /// 单会话（测试）时内部只有一个会话，`active` 恒指它。
    sessions: SessionManager,
    exit: bool,
    input: String,
    input_cursor: usize,
    // 已发送的提示词历史，供上/下键回放。
    history: Vec<String>,
    // None 表示不在历史浏览中；Some(i) 表示当前显示的是 history[i]。
    history_index: Option<usize>,
    // 进入历史浏览前暂存的未发送输入，用于向下回到末尾时恢复。
    history_draft: String,
    show_thinking: bool,
    // 是否展开工具调用的完整参数（默认收起，只显示一行摘要）。
    show_tool_args: bool,
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
    /// 打开中的会话选择器；`None` 表示未打开。
    session_picker: Option<SessionPicker>,
    /// `/session` 被触发、但会话列表尚未加载完成时置位。
    /// 由 TUI 主循环取走并加载目录后打开选择器。
    session_picker_requested: bool,
    /// `/rewind` 已确认的回溯点：被编辑消息的 UI `items` 下标。
    ///
    /// 回溯只作用于**最后一条用户消息**（`docs/session.md` 一.决策 4），
    /// 因此只需记 UI 下标；Agent 侧回退由 `rewind_last_user_turn` 确定性完成。
    /// 下次提交前先回退到这里，再作为全新一轮发送。
    rewind_target: Option<usize>,
    /// 输入框提示语覆盖：指令打开的编辑态（如回溯编辑）用它说明"Enter 重发 / Esc 取消"。
    /// 为空时用默认提示。任何普通输入都会清掉它。
    command_hint: Option<String>,
    /// 进行中的 `/login` 分步流程；`None` 表示不在登录态。
    login: Option<LoginFlow>,
    /// `/login` 落盘的目标配置文件覆盖；`None` 时用工作区默认路径。
    /// 存在的意义是让测试写到临时目录，而不是污染真实工作区。
    config_path: Option<std::path::PathBuf>,
    /// 鼠标拖动选区；`None` 表示当前没有选中任何文本。
    /// 由 `selection` 模块渲染高亮并提取文本，松开左键即复制。
    selection: Option<Selection>,
}

impl std::ops::Deref for App {
    type Target = super::session::Session;
    /// 前台会话字段的透明访问：`self.items` / `self.agent` / `self.waiting` …
    /// 全部解析到 `sessions.active()`（"当前会话的视图"）。
    fn deref(&self) -> &Self::Target {
        self.sessions.active()
    }
}

impl std::ops::DerefMut for App {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.sessions.active_mut()
    }
}

impl App {
    /// 用内置静态模型目录构造（默认）。
    ///
    /// 主要供测试与不关心目录注入的场景使用；主程序走 [`App::with_manager`]。
    #[allow(dead_code)]
    pub fn new(agent: Agent) -> Self {
        Self::with_catalog(agent, Arc::new(StaticCatalog::builtin()))
    }

    /// 注入模型目录构造。远端目录将来从这里传入，UI 无需改动。
    ///
    /// 会话目录用空实现兜底（不落盘、列表为空），供不关心会话切换的场景使用。
    #[allow(dead_code)]
    pub fn with_catalog(agent: Agent, catalog: Arc<dyn ModelCatalog>) -> Self {
        Self::with_catalogs(agent, catalog, Arc::new(EmptySessionCatalog))
    }

    /// 注入模型目录与会话目录构造（单会话，供测试使用）。
    #[allow(dead_code)]
    pub fn with_catalogs(
        agent: Agent,
        catalog: Arc<dyn ModelCatalog>,
        session_catalog: Arc<dyn SessionCatalog>,
    ) -> Self {
        Self::with_manager(
            SessionManager::single(agent, None, session_catalog, None),
            catalog,
        )
    }

    /// 用已装配好的多会话编排器构造（TUI / desktop 走这里）。
    pub fn with_manager(sessions: SessionManager, catalog: Arc<dyn ModelCatalog>) -> Self {
        Self {
            sessions,
            exit: false,
            input: String::new(),
            input_cursor: 0,
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            show_thinking: true,
            show_tool_args: false,
            message_cache: None,
            commands: CommandManager::new(DEFAULT_PREFIX),
            suggestions: Vec::new(),
            suggestion_index: 0,
            catalog,
            picker: None,
            picker_requested: false,
            session_picker: None,
            session_picker_requested: false,
            rewind_target: None,
            command_hint: None,
            login: None,
            config_path: None,
            selection: None,
        }
    }

    pub fn last_usage(&self) -> Option<&Usage> {
        self.last_usage.as_ref()
    }

    pub fn total_usage(&self) -> &Usage {
        &self.total_usage
    }

    pub fn is_compressing(&self) -> bool {
        self.compressing
    }

    pub fn context_usage(&self) -> Option<(u64, u64)> {
        self.context_usage
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

    /// 开始一次鼠标拖动选区（左键按下）。
    pub fn begin_selection(&mut self, position: Position) {
        self.selection = Some(Selection::new(position));
    }

    /// 延伸当前选区到新位置（拖动中）。
    pub fn extend_selection(&mut self, position: Position) {
        if let Some(selection) = self.selection.as_mut() {
            selection.extend(position);
        }
    }

    /// 结束当前选区（左键松开）：清空并返回选中内容（原地单击返回 `None`）。
    ///
    /// 文本提取需要缓冲区，故由调用方（渲染后拿到缓冲区的主循环）负责，
    /// 这里只负责把状态收走，避免旧选区残留到下一次拖动。
    pub fn take_selection(&mut self) -> Option<Selection> {
        self.selection.take()
    }

    /// 当前选区（供渲染层高亮）。
    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// 鼠标滚轮滚动：正数向下。与键盘 `PageUp`/`PageDown` 共用同一套滚动逻辑。
    pub fn scroll_lines(&mut self, delta: i32) {
        self.scroll_by(delta);
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
        // 登录流程进行中：Enter 提交的是"当前字段的值"，不是发给 AI 的 prompt。
        // 优先于其它一切分支——登录态下输入框被临时征用为字段编辑器。
        if self.login.is_some() {
            self.advance_login();
            return None;
        }
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
            CommandOutcome::Rewind => {
                // 不发送、不落历史：直接回退最后一条用户消息，把原文填回输入框。
                // 方案 A（`docs/session.md` 一.决策 4）：只做最新一条，故无需选择面板——
                // 没有"选择"这个动作，选择器无存在意义。
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.start_rewind_last_turn();
                return None;
            }
            CommandOutcome::SessionPicker => {
                // 不发送、不落历史：清空输入并请求主循环加载会话列表后打开选择器。
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.command_hint =
                    Some(" ↑↓ 选择 · Enter 切换 · Esc 取消 ".to_owned());
                self.session_picker_requested = true;
                return None;
            }
            CommandOutcome::Login => {
                // 不发送、不落历史：进入分步登录，输入框转为字段编辑器。
                self.input.clear();
                self.input_cursor = 0;
                self.suggestions.clear();
                self.start_login();
                return None;
            }
            CommandOutcome::Unknown => {}
        }

        // 若处于回溯编辑态（`/rewind` 后未提交），先同步回退 Agent 与界面，
        // 再作为全新一轮发送——保证 UI 视图与真实上下文始终一致。
        // Agent 侧回退最后一条用户消息（及其后的 assistant/tool 链），界面侧截到
        // 该条之前；随后本轮的 `push_message` 会把编辑后的内容作为新一条补上。
        if let Some(ui_index) = self.rewind_target.take() {
            // 内存（Agent）与磁盘日志一起回退到该用户消息之前（`Session` 内完成）。
            self.rewind_last_user_turn();
            self.items.truncate(ui_index);
            self.streaming_delta_start = None;
            self.message_cache = None;
            self.command_hint = None;
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
        self.sessions.active_mut().agent.take()
    }

    pub fn restore_agent(&mut self, agent: Agent) {
        let session = self.sessions.active_mut();
        session.agent = Some(agent);
        session.waiting = false;
        session.waiting_since = None;
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
            // 会话日志同步截尾（`docs/session.md` 一.决策 4），失败则记录但不阻断收尾。
            if let Some(prompt) = self.rewind_last_user_turn() {
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

    // ---- 历史回溯（`/rewind`，方案 A：只回退最新一条用户消息）----

    /// `/rewind` 的落点：直接回退**最后一条**用户消息，把原文填回输入框供编辑重发。
    ///
    /// 只做最新一条（`docs/session.md` 一.决策 4）——没有"选择"这个动作，
    /// 因此不需要选择面板：找最后一条用户消息、进编辑态即可。
    /// 被压缩掉的历史不在 `agent.messages` 里，天然不可回溯，语义自洽。
    ///
    /// 这里**只进入编辑态**（记录回退点 + 填输入框），真正的回退推迟到 `submit`：
    /// 这样编辑期间界面仍保留原对话，渲染层能把被编辑那条就地画成输入框
    /// （见 `ui.rs::draw_rewind_edit`），用户看得到上下文。
    pub fn start_rewind_last_turn(&mut self) {
        let Some(agent) = self.agent.as_ref() else {
            return;
        };
        // Agent 侧最后一条用户消息的原文。
        let Some(content) = agent
            .messages()
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::User { content } => Some(content.clone()),
                _ => None,
            })
        else {
            self.add_system_message("没有可回溯的消息。".to_owned());
            return;
        };
        // UI 侧最后一条用户条目下标：与 Agent 用户消息一一对应（每次提交各产生一条）。
        let Some(ui_index) = self.items.iter().rposition(|item| {
            matches!(item, Item::Message(m) if m.role == Role::User)
        }) else {
            self.add_system_message("没有可回溯的消息。".to_owned());
            return;
        };

        self.rewind_target = Some(ui_index);
        self.set_input(content);
        self.message_cache = None;
        // 进入"编辑重发"态：Enter 重新发送，Esc 取消本次改动。
        // 输入框由渲染层就地画到该消息的位置（见 `ui.rs::draw_rewind_edit`）。
        self.command_hint = Some(" Enter 重新发送 · Esc 取消本次修改 ".to_owned());
    }

    /// 指令编辑态的输入框提示覆盖（无则为 `None`）。
    pub fn command_hint(&self) -> Option<&str> {
        self.command_hint.as_deref()
    }

    /// 是否处于"回溯编辑重发"态（已进入编辑态、等待编辑后提交）。
    pub fn is_rewind_edit(&self) -> bool {
        self.rewind_target.is_some()
    }

    /// 回溯编辑态下正在被编辑的 UI 条目下标（渲染层据此把该消息就地画成输入框）。
    pub fn rewind_edit_item(&self) -> Option<usize> {
        self.rewind_target
    }

    /// 仅测试用：直接进入回溯编辑态并指向某条 UI 条目。
    #[cfg(test)]
    pub fn set_rewind_edit_for_test(&mut self, ui_index: usize) {
        self.rewind_target = Some(ui_index);
    }

    /// 取消本次回溯编辑：清掉编辑态与输入，Agent / 界面均不改动。
    /// 用户"不回车直接取消"的落点。
    pub fn cancel_rewind_edit(&mut self) {
        if self.rewind_target.take().is_some() {
            self.input.clear();
            self.input_cursor = 0;
            self.command_hint = None;
            self.suggestions.clear();
        }
    }

    // ---- 分步登录（`/login`）----
    //
    // 分步问答而非表单浮层：复用现有输入框与 `submit` 链路，不需要新的渲染态。
    // 代价是每步一次回车，收益是不碰 UI 层、可立即用（见对话记录）。

    /// 是否处于 `/login` 分步流程中（渲染层据此换输入框提示）。
    pub fn is_login(&self) -> bool {
        self.login.is_some()
    }

    /// 覆盖 `/login` 的落盘目标（默认写工作区 `.shirley/config.toml`）。
    /// 供测试注入临时路径，避免污染真实工作区。
    #[cfg(test)]
    pub fn set_config_path(&mut self, path: impl Into<std::path::PathBuf>) {
        self.config_path = Some(path.into());
    }

    /// `/login` 的当前阶段（供 UI / 测试查看）。
    #[cfg(test)]
    pub fn login_step(&self) -> Option<LoginStep> {
        self.login.as_ref().map(|flow| flow.step)
    }

    /// 进入分步登录：以当前配置为默认值，从 base_url 开始问。
    ///
    /// 输入框预填当前值，用户直接回车即"保持不变"；想改就编辑。
    pub fn start_login(&mut self) {
        let (base_url, api_key) = match self.agent.as_ref() {
            Some(agent) => {
                let config = agent.model_config();
                (config.base_url.clone(), config.api_key.clone())
            }
            None => (String::new(), None),
        };
        self.login = Some(LoginFlow {
            step: LoginStep::BaseUrl,
            base_url,
            api_key,
        });
        // 预填当前 base_url，回车即保留。
        self.prompt_login_step();
    }

    /// 取消登录：清掉流程与输入，Agent / 配置均不动。用户按 Esc 的落点。
    pub fn cancel_login(&mut self) {
        if self.login.take().is_some() {
            self.input.clear();
            self.input_cursor = 0;
            self.command_hint = None;
            self.suggestions.clear();
            self.add_system_message("已取消登录。".to_owned());
        }
    }

    /// 把当前阶段的默认值填进输入框，并更新提示语。
    fn prompt_login_step(&mut self) {
        let Some(step) = self.login.as_ref().map(|flow| flow.step) else {
            return;
        };
        let current = match step {
            LoginStep::BaseUrl => self
                .login
                .as_ref()
                .map(|flow| flow.base_url.clone())
                .unwrap_or_default(),
            LoginStep::ApiKey => self
                .login
                .as_ref()
                .and_then(|flow| flow.api_key.clone())
                .unwrap_or_default(),
            LoginStep::Model => self
                .agent
                .as_ref()
                .map(|agent| agent.model_config().model.clone())
                .unwrap_or_default(),
        };
        self.set_input(current);
        self.command_hint = Some(format!(" {} · Enter 确认 · Esc 取消 ", step.prompt()));
        self.add_system_message(format!("{}（回车保留当前值）：", step.prompt()));
    }

    /// 提交当前字段，推进到下一步；走完则落盘并热更新。
    fn advance_login(&mut self) {
        let Some(mut flow) = self.login.take() else {
            return;
        };
        let value = std::mem::take(&mut self.input);
        self.input_cursor = 0;
        self.suggestions.clear();

        match flow.step {
            LoginStep::BaseUrl => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    // 空端点无意义：留在本步重问，不推进。
                    self.add_system_message("base_url 不能为空，请重新输入。".to_owned());
                    self.login = Some(flow);
                    self.prompt_login_step();
                    return;
                }
                flow.base_url = trimmed.to_owned();
                flow.step = LoginStep::ApiKey;
                self.login = Some(flow);
                self.prompt_login_step();
            }
            LoginStep::ApiKey => {
                let trimmed = value.trim();
                // 空输入 = 无鉴权（本地服务常见），与"保留旧值"不同——这里明确清空。
                flow.api_key = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                flow.step = LoginStep::Model;
                self.login = Some(flow);
                self.prompt_login_step();
            }
            LoginStep::Model => {
                let trimmed = value.trim();
                let model = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                self.finish_login(&flow, model);
            }
        }
    }

    /// 落盘 + 热更新：写工作区配置，并让 Agent 立即用上新端点。
    fn finish_login(&mut self, flow: &LoginFlow, model: Option<String>) {
        self.command_hint = None;
        self.message_cache = None;

        // 热更新 Agent：端点 / 密钥立即生效（模型可选，未填则沿用当前）。
        if let Some(agent) = self.agent.as_mut() {
            agent.set_provider(flow.base_url.clone(), flow.api_key.clone());
            if let Some(model) = model.clone() {
                agent.set_model(model);
            }
        }

        // 落盘：写配置文件（先读后改再写，不动其它键）。默认工作区路径，
        // 测试可覆盖成临时路径（见 `set_config_path`）。
        let target = self
            .config_path
            .clone()
            .unwrap_or_else(|| crate::prompt::workspace_root().join(".shirley/config.toml"));
        let provider = crate::settings::ProviderSettings {
            protocol: None,
            base_url: Some(flow.base_url.clone()),
            api_key: flow.api_key.clone(),
            models_url: None,
            model: model.clone(),
            context_window_tokens: None,
        };
        match crate::settings::save_provider(&target, provider) {
            Ok(path) => {
                let model_note = model
                    .map(|m| format!("，模型 {m}"))
                    .unwrap_or_default();
                self.add_system_message(format!(
                    "已登录：{}（已写入 {}）{}",
                    flow.base_url,
                    path.display(),
                    model_note
                ));
            }
            Err(error) => {
                // 热更新已生效，只是没落盘——如实告诉用户，不假装成功。
                self.add_system_message(format!(
                    "已切换服务（{}），但写入配置失败：{error}",
                    flow.base_url
                ));
            }
        }
    }

    /// 把一条 `AgentEvent` 累加到当前前台会话（共享 `Session::apply_event`）。
    ///
    /// TUI 与 desktop 走同一条累加路径：事件处理只此一处，界面只读结果。
    /// `Err` 是运行期错误文案，渲染成错误条目。
    pub fn apply_event(&mut self, event: Result<AgentEvent, String>) {
        self.sessions.active_mut().apply_event(event);
        self.message_cache = None;
    }

    /// 追加一条消息条目（并失效渲染缓存）。
    fn push_message(&mut self, role: Role, content: String, thinking: bool) {
        self.sessions.active_mut().push_message(role, content, thinking);
        self.message_cache = None;
    }

    /// 从一条落库消息重建条目（测试与历史回放用；`apply_event` 内部走 `Session`）。
    pub fn add_message(&mut self, message: Message) {
        self.sessions.active_mut().add_message(message);
        self.message_cache = None;
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

    // ---- 会话选择器（`/session`）----

    /// 主循环取走"需要加载会话列表"的请求。
    pub fn take_session_picker_request(&mut self) -> bool {
        std::mem::take(&mut self.session_picker_requested)
    }

    /// 会话目录句柄（供主循环加载列表）。
    pub fn session_catalog(&self) -> Arc<dyn SessionCatalog> {
        self.sessions.catalog()
    }

    /// 当前（前台）会话名（供状态展示）。
    pub fn current_session(&self) -> Option<&str> {
        self.sessions.active_name()
    }

    /// 用加载好的会话列表打开选择器，高亮当前会话。
    ///
    /// 列表首项固定插入「＋ 新建会话」哨兵，让"切换"与"新建"在同一个面板里完成。
    pub fn open_session_picker(&mut self, mut entries: Vec<SessionEntry>) {
        let current = self.sessions.active_name().map(str::to_owned);
        // 哨兵项置顶：name 为空串代表"新建"。
        let mut all = vec![SessionEntry {
            name: SessionPicker::NEW_NAME.to_owned(),
            label: "＋ 新建会话".to_owned(),
            preview: String::new(),
            modified_ms: 0,
            turns: 0,
        }];
        all.append(&mut entries);
        let selected = current
            .as_ref()
            .and_then(|name| all.iter().position(|e| &e.name == name))
            .unwrap_or(0);
        self.session_picker = Some(SessionPicker {
            entries: all,
            selected,
            current,
        });
        self.message_cache = None;
    }

    pub fn session_picker(&self) -> Option<&SessionPicker> {
        self.session_picker.as_ref()
    }

    /// 选择器上下移动高亮（不绕回，到边界停住——列表含新建入口，绕回易误触）。
    pub fn session_picker_move(&mut self, delta: i32) {
        let Some(picker) = self.session_picker.as_mut() else {
            return;
        };
        let last = picker.entries.len().saturating_sub(1) as i32;
        let next = (picker.selected as i32 + delta).clamp(0, last);
        picker.selected = next as usize;
    }

    /// 确认选择：新建或切换到目标会话，重建界面与 Agent 上下文。返回结果说明。
    ///
    /// 返回 `Some(说明文案)` 表示已切换；`None` 表示面板未打开或切换失败
    /// （失败时已通过系统消息提示）。
    pub fn session_picker_confirm(&mut self) -> Option<String> {
        let picker = self.session_picker.take()?;
        self.message_cache = None;
        self.command_hint = None;
        let entry = picker.entries.get(picker.selected).cloned()?;

        // 编排交给 `SessionManager`：新建 / 切换都只挪 `active` 指针，
        // **不重建 Agent**（已加载的会话直接复用其就绪 Agent；未加载的经工厂恢复）。
        let result = if entry.name == SessionPicker::NEW_NAME {
            self.sessions.create_new(None)
        } else {
            self.sessions.switch_to(&entry.name)
        };
        let name = match result {
            Ok(name) => name,
            Err(error) => {
                self.add_system_message(format!("切换会话失败：{error}"));
                return None;
            }
        };
        // 会话已换：界面重放新会话历史，并重置与会话绑定的统计量。
        self.rebuild_items_from_agent();
        self.reset_session_stats();
        let message = format!("已切换到会话：{name}");
        self.add_system_message(message.clone());
        Some(message)
    }

    /// 取消选择。
    pub fn session_picker_cancel(&mut self) {
        if self.session_picker.take().is_some() {
            self.message_cache = None;
        }
        self.command_hint = None;
    }

    /// 会话选择器是否打开（主循环据此把按键导向面板而非输入框）。
    pub fn is_session_picker_open(&self) -> bool {
        self.session_picker.is_some()
    }

    /// 从 Agent 当前消息表重放界面条目（切换会话后调用）。
    ///
    /// 界面条目是 Agent 消息的视图；换会话等于换了整份历史，视图必须整体重建，
    /// 不能增量。`add_message` 天然跳过 system / tool（与展示口径一致）。
    pub fn rebuild_items_from_agent(&mut self) {
        self.items.clear();
        self.streaming_delta_start = None;
        self.scroll = 0;
        self.auto_scroll = true;
        let messages: Vec<Message> = self
            .agent
            .as_ref()
            .map(|agent| agent.messages().to_vec())
            .unwrap_or_default();
        for message in messages {
            self.add_message(message);
        }
        self.message_cache = None;
    }

    /// 切换会话后重置与会话绑定的统计（usage / 上下文占用）。
    fn reset_session_stats(&mut self) {
        self.last_usage = None;
        self.total_usage = Usage::default();
        self.context_usage = None;
        self.compressing = false;
    }

    /// 输入变化时重算候选（在 push/insert/pop/delete/move 等之后统一调用）。
    pub fn on_input_changed(&mut self) {
        // 回溯编辑态与登录态下都保留各自的提示——用户改动的是"待重发内容"或
        // "正在填的字段"，这两种态仍然成立；其余情况一旦改动输入即恢复默认提示。
        if !self.is_rewind_edit() && !self.is_login() {
            self.command_hint = None;
        }
        self.refresh_suggestions();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shirley_agent_sdk::{ModelConfig, ModelProtocol};

    fn app() -> App {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        App::new(Agent::builder().model_config(config).build().unwrap())
    }

    /// 测试用 App：把 `/login` 落盘目标指向唯一临时文件，避免污染真实工作区
    /// （并发的测试若都写同一文件还会互相截断）。
    fn app_with_temp_config(tag: &str) -> App {
        let mut app = app();
        let dir = std::env::temp_dir().join(format!(
            "shirley_login_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        app.set_config_path(dir.join("config.toml"));
        app
    }

    #[test]
    fn session_command_requests_picker_without_sending() {
        let mut app = app();
        for ch in "/session".chars() {
            app.push_input(ch);
        }
        assert!(app.submit().is_none(), "/session 不应作为 prompt 发送");
        assert!(app.take_session_picker_request(), "应请求加载会话列表");
        assert!(!app.is_waiting(), "不应进入等待回复状态");
        assert!(app.input().is_empty(), "输入应被清空");
    }

    #[test]
    fn session_picker_new_entry_is_first_and_confirms_to_new_session() {
        let mut app = app();
        app.open_session_picker(vec![SessionEntry {
            name: "20240101-000000".into(),
            label: "20240101-000000".into(),
            preview: "旧对话".into(),
            modified_ms: 0,
            turns: 1,
        }]);
        assert!(app.is_session_picker_open());
        // 首项固定是「新建」哨兵。
        assert_eq!(app.session_picker().unwrap().entries[0].name, "");
        // 无当前会话 → 高亮新建项。
        assert_eq!(app.session_picker().unwrap().selected, 0);
        app.session_picker_cancel();
        assert!(!app.is_session_picker_open());
    }

    #[test]
    fn session_picker_move_does_not_wrap() {
        let mut app = app();
        app.open_session_picker(vec![
            SessionEntry { name: "a".into(), label: "a".into(), preview: String::new(), modified_ms: 0, turns: 0 },
            SessionEntry { name: "b".into(), label: "b".into(), preview: String::new(), modified_ms: 0, turns: 0 },
        ]);
        // 共 3 项（新建 + a + b）。
        app.session_picker_move(-1);
        assert_eq!(app.session_picker().unwrap().selected, 0, "到顶应停住，不绕回");
        app.session_picker_move(5);
        assert_eq!(app.session_picker().unwrap().selected, 2, "到底应停住，不绕回");
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

    // ---- 分步登录（`/login`）----

    #[test]
    fn login_command_enters_flow_without_sending() {
        let mut app = app();
        for ch in "/login".chars() {
            app.push_input(ch);
        }
        assert!(app.submit().is_none(), "/login 不应作为 prompt 发送");
        assert!(app.is_login(), "应进入登录流程");
        assert_eq!(app.login_step(), Some(LoginStep::BaseUrl));
        assert!(!app.is_waiting(), "不应进入等待回复状态");
        // 输入框预填当前 base_url，回车即保留。
        assert_eq!(app.input(), "http://localhost");
    }

    #[test]
    fn login_collects_fields_step_by_step() {
        let mut app = app_with_temp_config("collect");
        app.start_login();

        // 第一步：改 base_url。
        app.set_input("http://new-endpoint".into());
        app.submit();
        assert_eq!(app.login_step(), Some(LoginStep::ApiKey));
        assert_eq!(app.input(), "", "api_key 默认空");

        // 第二步：填 key。
        app.set_input("sk-secret".into());
        app.submit();
        assert_eq!(app.login_step(), Some(LoginStep::Model));
        assert_eq!(app.input(), "test", "模型默认当前值");

        // 第三步：改模型 → 结束。
        app.set_input("new-model".into());
        app.submit();
        assert!(!app.is_login(), "走完应退出登录态");
        let agent = app.take_agent().unwrap();
        assert_eq!(agent.model_config().base_url, "http://new-endpoint");
        assert_eq!(agent.model_config().api_key.as_deref(), Some("sk-secret"));
        assert_eq!(agent.model_config().model, "new-model");
    }

    #[test]
    fn login_empty_base_url_is_rejected_and_stays() {
        let mut app = app();
        app.start_login();
        app.set_input("   ".into());
        app.submit();
        // 空端点无意义：留在本步。
        assert_eq!(app.login_step(), Some(LoginStep::BaseUrl));
        assert!(app.is_login());
    }

    #[test]
    fn login_empty_api_key_means_no_auth() {
        let mut app = app_with_temp_config("noauth");
        app.start_login();
        app.set_input("http://x".into());
        app.submit();
        // api_key 直接回车 → 无鉴权。
        app.submit();
        app.set_input("m".into());
        app.submit();
        let agent = app.take_agent().unwrap();
        assert_eq!(agent.model_config().api_key, None);
    }

    #[test]
    fn login_cancel_keeps_config_untouched() {
        let mut app = app();
        app.start_login();
        app.set_input("http://changed".into());
        app.cancel_login();
        assert!(!app.is_login());
        assert!(app.input().is_empty(), "取消应清空输入");
        let agent = app.take_agent().unwrap();
        assert_eq!(
            agent.model_config().base_url, "http://localhost",
            "取消不应改变配置"
        );
    }

    #[test]
    fn login_enter_keeps_existing_values() {
        let mut app = app_with_temp_config("keep");
        app.start_login();
        // 三步全部回车 → 保留原值。
        app.submit(); // base_url 保留
        app.submit(); // api_key 保留（None → 空 → None）
        app.submit(); // model 保留
        let agent = app.take_agent().unwrap();
        assert_eq!(agent.model_config().base_url, "http://localhost");
        assert_eq!(agent.model_config().model, "test");
    }

    fn agent_with_users(messages: Vec<Message>) -> Agent {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        Agent::builder().model_config(config).messages(messages).build().unwrap()
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
    fn rewind_command_fills_last_user_message() {
        let mut app = App::new(agent_with_users(vec![
            Message::User { content: "first".into() },
            Message::User { content: "second".into() },
        ]));
        // 让 UI 也有对应的两条用户条目。
        app.add_message(Message::User { content: "first".into() });
        app.add_message(Message::User { content: "second".into() });

        app.start_rewind_last_turn();
        // 只回退最新一条：输入框应填 "second"，编辑点指向第 1 条 UI 条目。
        assert_eq!(app.input(), "second", "应填回最后一条用户消息");
        assert!(app.is_rewind_edit(), "应进入回溯编辑态");
        assert_eq!(app.rewind_edit_item(), Some(1), "应定位到被编辑的消息条目");
        assert!(app.command_hint().is_some(), "应给出重发提示");
    }

    #[test]
    fn submit_after_rewind_rolls_back_agent_and_ui() {
        let mut app = App::new(agent_with_users(vec![
            Message::User { content: "first".into() },
            Message::User { content: "second".into() },
        ]));
        app.add_message(Message::User { content: "first".into() });
        app.add_message(Message::User { content: "second".into() });

        app.start_rewind_last_turn(); // 回退 "second"
        // 编辑后提交。
        app.push_input('!');
        assert_eq!(app.submit().as_deref(), Some("second!"));

        // 界面只剩回退点之前的条目 + 新提交的这条。
        let user_items = app
            .items()
            .iter()
            .filter_map(|i| match i {
                Item::Message(m) if m.role == Role::User => Some(m.content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(user_items, vec!["first".to_owned(), "second!".to_owned()]);
    }

    #[test]
    fn rewind_without_user_message_is_noop() {
        let mut app = App::new(agent_with_users(vec![]));
        app.start_rewind_last_turn();
        assert!(!app.is_rewind_edit(), "无可回溯消息不应进入编辑态");
        assert!(app.input().is_empty());
    }

    #[test]
    fn rewind_command_starts_edit_without_sending() {
        let mut app = App::new(agent_with_users(vec![Message::User {
            content: "only".into(),
        }]));
        app.add_message(Message::User { content: "only".into() });
        for ch in "/rewind".chars() {
            app.push_input(ch);
        }
        assert!(app.submit().is_none(), "/rewind 不应作为 prompt 发送");
        assert!(app.is_rewind_edit(), "应进入回溯编辑态");
        assert!(!app.is_waiting(), "不应进入等待回复状态");
        assert_eq!(app.input(), "only", "应填回最后一条用户消息");
        assert!(app.command_hint().is_some(), "应给出重发提示");
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
        assert_eq!(app.input(), "first");

        // Esc：取消本次改动——清输入、退出编辑态，且不发送。
        app.cancel_rewind_edit();
        assert!(!app.is_rewind_edit(), "取消后应退出编辑态");
        assert!(app.input().is_empty(), "取消应清空输入");
        assert!(app.command_hint().is_none(), "取消应清掉提示");

        // 再来一次，这次编辑后回车重发。
        app.start_rewind_last_turn();
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
                    .build()
                    .unwrap(),
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
                .build()
                .unwrap(),
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
