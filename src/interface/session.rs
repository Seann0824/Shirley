//! 应用层多会话运行时容器（`docs/multi-session.md` 决策 4）。
//!
//! 这是应用层的**唯一编排入口**：把「会话从哪来」（`SessionCatalog`）、
//! 「每个会话跑到哪了」（[`Session`]）、「前台是谁」（`SessionManager::active`）
//! 三件事收在一处。
//!
//! 与 [`crate::session`]（存储 / 目录层）不同：那一层只管"日志文件怎么读写、
//! 会话怎么列出来"；本模块管"每个会话在内存里长什么样、谁在前台"。一个
//! [`Session`] 自持一个独立 [`Agent`]（决策 1：多会话 = 多个独立 `Agent`，
//! 不是"一个 Agent 分时"），因此不同会话的召回库 / 任务账本 / 会话日志天然隔离。
//!
//! 身份住在容器上、`Agent` 保持匿名（决策 4）：工厂 `build_agent` 造出匿名
//! `Agent`，塞进 `Session` 时贴一次 `name`——避免"`Agent` 里一个 id、`Session`
//! 里一个 id"两个真相源。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use shirley_agent_sdk::{Agent, AgentEvent, Message, Usage};
use tokio::sync::broadcast;

use super::app::{ChatMessage, Item, Role, ToolCallView};
use crate::bootstrap::AgentFactory;
use crate::memory::MemoryRuntime;
use crate::session::{SessionCatalog, SessionStore};

/// 事件扇出通道容量。运行中订阅者若落后超过此数会 `Lagged`，由订阅方（desktop
/// pump）跳过、靠重发快照重对齐（见 `docs/multi-session.md`）。
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// 一个会话 = 一个运行时容器。
///
/// 它持有该会话**独占**的 `Agent` 与全部"随会话走"的界面状态（消息条目 /
/// 滚动位置 / 用量 / 本轮状态……）。前台切换只是把 [`SessionManager::active`]
/// 指针挪到它身上——**不重建 `Agent`**（重建会丢后台上下文）。
pub struct Session {
    /// 会话名（复用 [`crate::session::SessionEntry::name`]）；`None` 表示尚未落盘的
    /// 内存会话（惰性新建、物化前）。
    pub name: Option<String>,
    /// 该会话独占的 `Agent`。一轮运行期间被 `take` 出去移入后台任务，结束后放回
    /// （与旧 `take_agent` / `restore_agent` 同款手法，但**每个会话各一份**）。
    pub agent: Option<Agent>,
    /// 该会话的消息条目（渲染视图）。
    pub items: Vec<Item>,
    pub waiting: bool,
    pub waiting_since: Option<Instant>,
    pub scroll: usize,
    pub auto_scroll: bool,
    pub max_scroll: usize,
    pub last_usage: Option<Usage>,
    pub total_usage: Usage,
    pub compressing: bool,
    pub context_usage: Option<(u64, u64)>,
    /// 本轮流式回复起始的 `items` 下标（收尾时据此截断半截流式内容）。
    pub streaming_delta_start: Option<usize>,
    /// 本轮回复开始时 UI `items` 的长度，打断时据此回退界面。
    pub ui_turn_start: Option<usize>,
    /// 本轮提交的输入，打断后退回输入框供修改重发。
    pub turn_prompt: Option<String>,
    /// 用户按 Esc 请求打断当前回复（由主循环读取后触发取消）。
    pub interrupt_requested: bool,
    /// 该会话的事件扇出通道（多会话：UI 切到会话 → 订阅，切走 → 退订）。
    ///
    /// 通道归 `Session` 自己持有（与"每个 `Session` 自持 `Agent`"对称，见
    /// `docs/multi-session.md` 决策 2）：`SessionManager` 只负责把每个
    /// `AgentEvent` 喂给对应会话，不持有通道。
    events: broadcast::Sender<Result<AgentEvent, String>>,
    /// 该会话是否正在跑一轮（`Agent` 被移入后台任务）。与 `agent.is_none()` 同义，
    /// 单独记一份是为了让状态转移显式。
    pub running: bool,
    /// 该会话的持久化存储（`None` = 不落盘，供测试）。
    ///
    /// 落库由 [`Session::apply_event`] 事件驱动：收到 SDK 的 `MessageAdded` 即
    /// `append`。会话持久化完全在应用层，SDK 不再持有会话日志。
    store: Option<Arc<dyn SessionStore>>,
    /// 该会话的记忆运行时（`None` = 无记忆，供测试 / 无配置场景）。
    ///
    /// 每会话一份（`docs/memory.md` §4.2）：query 槽独立，多会话并发互不干扰。
    /// 驱动方在发起一轮前 `set_query`（按本轮用户输入做相关检索），会话结束
    /// 交给 curator。
    pub memory: Option<Arc<MemoryRuntime>>,
}

impl Session {
    /// 用现成的 `Agent` 起一个会话。
    ///
    /// `store` 是该会话的持久化存储（`None` = 不落盘）；落库走
    /// [`Session::apply_event`] 的事件驱动路径。
    pub fn new(
        agent: Agent,
        name: Option<String>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
    ) -> Self {
        Self {
            name,
            agent: Some(agent),
            items: Vec::new(),
            waiting: false,
            waiting_since: None,
            scroll: 0,
            auto_scroll: true,
            max_scroll: 0,
            last_usage: None,
            total_usage: Usage::default(),
            compressing: false,
            context_usage: None,
            streaming_delta_start: None,
            ui_turn_start: None,
            turn_prompt: None,
            interrupt_requested: false,
            events: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            running: false,
            store,
            memory,
        }
    }

    /// 订阅本会话的事件流（多会话：UI 切到该会话时订阅，切走时丢弃句柄退订）。
    pub fn subscribe(&self) -> broadcast::Receiver<Result<AgentEvent, String>> {
        self.events.subscribe()
    }

    /// 发布一条事件给所有订阅者（无订阅者时静默丢弃——会话照常累加）。
    pub fn publish(&self, event: Result<AgentEvent, String>) {
        let _ = self.events.send(event);
    }

    /// 把一条 `AgentEvent` 累加到本会话的视图（消息条目 / 用量 / 压缩态 / 上下文）。
    ///
    /// 这是 TUI 与 desktop **共享**的累加入口：谁驱动 `run_stream`，就把事件喂给
    /// 这里，界面只读结果。`Err` 分支是运行期错误文案（渲染成错误条目）。
    pub fn apply_event(&mut self, event: Result<AgentEvent, String>) {
        match event {
            Ok(AgentEvent::MessageAdded(message)) => {
                // 事件驱动落库：SDK 只发事件，落盘由应用层负责（system 不入日志）。
                self.persist_message(&message);
                // 用户消息的视图条目由驱动方在发起一轮时记入（见 `begin_turn`），
                // 这里只补 assistant / tool / summary 的视图。
                if !matches!(&message, Message::User { .. }) {
                    if matches!(&message, Message::Assistant { .. }) {
                        self.finish_streaming_deltas();
                    }
                    self.add_message(message);
                }
            }
            Ok(AgentEvent::ContentDelta(delta)) => self.append_streaming_delta(delta, false),
            Ok(AgentEvent::ReasoningDelta(delta)) => self.append_streaming_delta(delta, true),
            Ok(AgentEvent::Usage(usage)) => self.record_usage(usage),
            Ok(AgentEvent::CompressionStarted) => self.start_compression(),
            Ok(AgentEvent::CompressionFinished) => self.finish_compression(),
            Ok(AgentEvent::ContextUsage {
                used_tokens,
                limit_tokens,
            }) => self.record_context_usage(used_tokens, limit_tokens),
            Err(error) => self.add_error(error),
            // Tool / run-finished 事件不产生额外条目（工具组由 MessageAdded 落库时建立）。
            Ok(AgentEvent::ToolStarted { .. })
            | Ok(AgentEvent::ToolFinished { .. })
            | Ok(AgentEvent::Finished(_)) => {}
        }
    }

    /// 当前视图快照：按 UI 条目顺序导出可重建的纯数据（`Item` → [`ItemWire`]）。
    ///
    /// UI 订阅一个**已在运行 / 已积累历史**的会话时，先拿快照重建视图，再叠加
    /// 之后的事件——避免"切回来看到空白、只有新增量"。
    pub fn snapshot(&self) -> Vec<ItemWire> {
        self.items.iter().map(ItemWire::from_item).collect()
    }

    /// 从当前 `Agent` 的消息表重建视图条目（会话从磁盘加载后调用）。
    ///
    /// 视图条目是 Agent 消息的展示投影；从磁盘恢复的会话其 `Agent` 已带全量历史，
    /// 但 `items` 还是空的——重建一次，快照 / 渲染才有内容。
    pub fn rebuild_items_from_agent(&mut self) {
        let items = self
            .agent
            .as_ref()
            .map(|agent| Self::items_from_messages(agent.messages()))
            .unwrap_or_default();
        self.items = items;
        self.streaming_delta_start = None;
    }

    /// 从一条落库消息重建视图条目（运行中订阅已结束会话 / 切换会话时用）。
    pub fn items_from_messages(messages: &[Message]) -> Vec<Item> {
        let mut items = Vec::new();
        for message in messages {
            match message {
                Message::User { content } => items.push(Item::Message(ChatMessage {
                    role: Role::User,
                    content: content.clone(),
                    thinking: false,
                })),
                Message::Assistant {
                    content,
                    reasoning_content,
                    tool_calls,
                } => {
                    if let Some(reasoning) = reasoning_content
                        .as_ref()
                        .filter(|text| !text.trim().is_empty())
                    {
                        items.push(Item::Message(ChatMessage {
                            role: Role::Assistant,
                            content: reasoning.clone(),
                            thinking: true,
                        }));
                    }
                    if let Some(content) = content.as_ref().filter(|text| !text.is_empty()) {
                        items.push(Item::Message(ChatMessage {
                            role: Role::Assistant,
                            content: content.clone(),
                            thinking: false,
                        }));
                    }
                    if !tool_calls.is_empty() {
                        items.push(Item::Tools(super::app::ToolGroup {
                            calls: tool_calls
                                .iter()
                                .map(|call| ToolCallView {
                                    name: call.name.clone(),
                                    arguments: call.arguments.clone(),
                                })
                                .collect(),
                        }));
                    }
                }
                Message::ContextSummary { content } => items.push(Item::Message(ChatMessage {
                    role: Role::Summary,
                    content: content.clone(),
                    thinking: false,
                })),
                Message::Tool { .. } | Message::System { .. } => {}
            }
        }
        items
    }

    pub(crate) fn push_message(&mut self, role: Role, content: String, thinking: bool) {
        self.items.push(Item::Message(ChatMessage {
            role,
            content,
            thinking,
        }));
    }

    fn append_streaming_delta(&mut self, delta: String, thinking: bool) {
        if delta.is_empty() {
            return;
        }
        if self.streaming_delta_start.is_some() {
            if let Some(Item::Message(message)) = self.items.last_mut()
                && message.role == Role::Assistant
                && message.thinking == thinking
            {
                message.content.push_str(&delta);
                return;
            }
        } else {
            self.streaming_delta_start = Some(self.items.len());
        }
        self.push_message(Role::Assistant, delta, thinking);
    }

    fn finish_streaming_deltas(&mut self) {
        if let Some(start) = self.streaming_delta_start.take() {
            self.items.truncate(start);
        }
    }

    fn start_tool_calls(&mut self, calls: Vec<ToolCallView>) {
        if calls.is_empty() {
            return;
        }
        self.items.push(Item::Tools(super::app::ToolGroup { calls }));
    }

    pub(crate) fn add_message(&mut self, message: Message) {
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

    fn add_error(&mut self, error: String) {
        self.compressing = false;
        self.push_message(Role::Error, error, false);
    }

    /// 把一条消息追加到会话日志（事件驱动落库）。
    ///
    /// 落盘失败记入错误条目但不阻断本轮：`docs/session.md` 二.2 把持久化失败
    /// 当作错误暴露，但 UI 不该因此中断整轮对话。
    fn persist_message(&mut self, message: &Message) {
        let Some(store) = self.store.clone() else {
            return;
        };
        if let Err(error) = store.append(message) {
            self.add_error(format!("会话落盘失败：{error}"));
        }
    }

    /// 回退最后一条用户消息：内存（`Agent`）与磁盘日志一起回退。
    ///
    /// "回退最后一轮"是**会话 / UI 业务语义**（谁定义"一轮"、是否连带丢弃其后的
    /// assistant / tool 链），故判定放在应用层；SDK 只提供原子能力
    /// [`Agent::truncate_messages`]（尾截断 + 维护不变量）。日志不含 system，
    /// 故换算长度时减去置顶的那条。
    pub fn rewind_last_user_turn(&mut self) -> Option<String> {
        let agent = self.agent.as_mut()?;
        let index = agent
            .messages()
            .iter()
            .rposition(|m| matches!(m, Message::User { .. }))?;
        let content = match &agent.messages()[index] {
            Message::User { content } => content.clone(),
            _ => unreachable!("rposition guarantees a User message"),
        };
        // 截到该用户消息之前（丢弃这一轮 user 及其后的 assistant / tool 链）。
        let new_len = agent.truncate_messages(index);
        let has_system = matches!(agent.messages().first(), Some(Message::System { .. }));
        let log_len = new_len.saturating_sub(usize::from(has_system));
        let store = self.store.clone();
        if let Some(store) = store
            && let Err(error) = store.truncate(log_len)
        {
            self.add_error(format!("会话日志截尾失败：{error}"));
        }
        Some(content)
    }

    fn record_usage(&mut self, usage: Usage) {
        self.total_usage = self.total_usage + usage;
        self.last_usage = Some(usage);
    }

    fn start_compression(&mut self) {
        self.compressing = true;
    }

    fn finish_compression(&mut self) {
        self.compressing = false;
        self.context_usage = None;
    }

    fn record_context_usage(&mut self, used_tokens: u64, limit_tokens: u64) {
        self.context_usage = Some((used_tokens, limit_tokens));
    }
}

/// UI 条目的中性线格式（`docs/multi-session.md` 决策 4）：由 [`Item`] 映射而来，
/// 供 UI 订阅已积累历史的会话时重建视图。TUI 不消费它（直接读 [`Item`]）；
/// desktop 侧再转成前端 DTO（`desktop::wire::ItemWire`）。
#[derive(Debug, Clone)]
pub enum ItemWire {
    /// 一条消息条目（角色 / 正文 / 是否思考块）。
    Message {
        role: Role,
        content: String,
        thinking: bool,
    },
    /// 一组工具调用。
    Tools { calls: Vec<ToolCallWire> },
}

/// [`ItemWire::Tools`] 里的单次工具调用。
#[derive(Debug, Clone)]
pub struct ToolCallWire {
    pub name: String,
    pub arguments: String,
}

impl ItemWire {
    /// 从 UI 条目映射（`Item` → 线格式）。
    pub fn from_item(item: &Item) -> Self {
        match item {
            Item::Message(message) => ItemWire::Message {
                role: message.role,
                content: message.content.clone(),
                thinking: message.thinking,
            },
            Item::Tools(group) => ItemWire::Tools {
                calls: group
                    .calls
                    .iter()
                    .map(|call| ToolCallWire {
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .collect(),
            },
        }
    }
}

/// 全局唯一的会话编排器：持有全部 [`Session`]、前台指针，以及"造 `Agent` 的工厂"
/// 与"会话从哪来"的目录。
///
/// `factory` / `catalog` 为 `None` 时退化为"单会话、不可新建/切换"——供不关心
/// 多会话的测试使用（如 `App::new`）。
/// 一次 curation 的全部输入（`spawn_curation` 与 `finish` 共用）。
struct CurationInputs {
    factory: Arc<AgentFactory>,
    memory: Arc<MemoryRuntime>,
    messages: Vec<Message>,
    session_ref: String,
}

pub struct SessionManager {
    sessions: HashMap<u64, Session>,
    /// 前台会话的 key。
    active: u64,
    next_id: u64,
    catalog: Arc<dyn SessionCatalog>,
    factory: Option<Arc<AgentFactory>>,
}

impl SessionManager {
    /// 用一份现成 `Agent` 起一个只含单会话的 manager（无工厂，不能新建/切换）。
    pub fn single(
        agent: Agent,
        name: Option<String>,
        catalog: Arc<dyn SessionCatalog>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
    ) -> Self {
        let mut sessions = HashMap::new();
        sessions.insert(0, Session::new(agent, name, store, memory));
        Self {
            sessions,
            active: 0,
            next_id: 1,
            catalog,
            factory: None,
        }
    }

    /// 可造 `Agent` 的 manager：交给它工厂与会话目录，支持新建 / 切换。
    pub fn with_factory(
        agent: Agent,
        name: Option<String>,
        catalog: Arc<dyn SessionCatalog>,
        factory: Arc<AgentFactory>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
    ) -> Self {
        let mut sessions = HashMap::new();
        sessions.insert(0, Session::new(agent, name, store, memory));
        Self {
            sessions,
            active: 0,
            next_id: 1,
            catalog,
            factory: Some(factory),
        }
    }

    /// 前台会话（只读）。
    pub fn active(&self) -> &Session {
        self.sessions
            .get(&self.active)
            .expect("active session must exist")
    }

    /// 前台会话（可变）。
    pub fn active_mut(&mut self) -> &mut Session {
        self.sessions
            .get_mut(&self.active)
            .expect("active session must exist")
    }

    /// 前台会话名。
    pub fn active_name(&self) -> Option<&str> {
        self.active().name.as_deref()
    }

    /// 会话目录句柄（供主循环加载列表 / 新建切换）。
    pub fn catalog(&self) -> Arc<dyn SessionCatalog> {
        Arc::clone(&self.catalog)
    }

    /// 已加载会话里是否已有该名字（用于切换时复用现成 `Agent`，不重建）。
    fn id_of_name(&self, name: &str) -> Option<u64> {
        self.sessions
            .iter()
            .find(|(_, session)| session.name.as_deref() == Some(name))
            .map(|(id, _)| *id)
    }

    /// 插入一个现成 `Agent` 的会话，返回其 key（不改变前台指针）。
    fn insert(
        &mut self,
        agent: Agent,
        name: Option<String>,
        store: Option<Arc<dyn SessionStore>>,
        memory: Option<Arc<MemoryRuntime>>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.sessions
            .insert(id, Session::new(agent, name, store, memory));
        id
    }

    /// 切换到指定会话（决策 4 / 决策 9）。
    ///
    /// 已加载 → **只挪前台指针**（不重建 `Agent`，保住后台上下文）；未加载 →
    /// 从目录 `open` 日志、经工厂 `build_agent` 恢复工作集后插入再切前台。
    /// 返回该会话名。
    pub fn switch_to(&mut self, name: &str) -> Result<String, String> {
        // 离开前台即视为该会话"本轮结束"——切走后触发一次增量 curation
        // （`docs/memory.md` §5.1；旧会话仍留在内存，其 `Agent` 消息可读）。
        let previous = self.active().name.clone();
        if let Some(id) = self.id_of_name(name) {
            self.active = id;
            if let Some(previous) = previous.filter(|p| p != name) {
                self.spawn_curation(&previous);
            }
            return Ok(name.to_owned());
        }
        let factory = self
            .factory
            .as_ref()
            .ok_or_else(|| "当前会话目录不支持切换".to_owned())?;
        let store = self
            .catalog
            .open(name)
            .map_err(|error| error.to_string())?;
        // 会话持久化在应用层：先 load 日志，再交给工厂起 `Agent`（system 现生成置顶）。
        let log = store.load().map_err(|error| error.to_string())?;
        let built = factory
            .build_agent(log)
            .map_err(|error| error.to_string())?;
        let id = self.insert(
            built.agent,
            Some(name.to_owned()),
            Some(store),
            Some(built.memory),
        );
        // 从磁盘恢复的会话：用其历史重建视图条目，快照 / 渲染才有内容。
        if let Some(session) = self.sessions.get_mut(&id) {
            session.rebuild_items_from_agent();
        }
        self.active = id;
        if let Some(previous) = previous.filter(|p| p != name) {
            self.spawn_curation(&previous);
        }
        Ok(name.to_owned())
    }

    /// 新建（惰性）会话并切过去，返回新会话名。`title` 为空 = 匿名。
    ///
    /// 惰性：此刻只定名、不落盘，发首条消息才真正建文件（`create_lazy`）。
    pub fn create_new(&mut self, title: Option<&str>) -> Result<String, String> {
        // 与 `switch_to` 同理：新建并切走 = 旧会话本轮结束，触发一次 curation。
        let previous = self.active().name.clone();
        let factory = self
            .factory
            .as_ref()
            .ok_or_else(|| "当前会话目录不支持新建".to_owned())?;
        let (entry, store) = self
            .catalog
            .create_lazy(title)
            .map_err(|error| error.to_string())?;
        // 惰性会话此刻为空：load 得空工作集，首条消息落盘时才物化文件。
        let log = store.load().map_err(|error| error.to_string())?;
        let built = factory
            .build_agent(log)
            .map_err(|error| error.to_string())?;
        let id = self.insert(
            built.agent,
            Some(entry.name.clone()),
            Some(store),
            Some(built.memory),
        );
        self.active = id;
        if let Some(previous) = previous.filter(|p| p != entry.name.as_str()) {
            self.spawn_curation(&previous);
        }
        Ok(entry.name)
    }

    /// 删除一个会话（内存侧）。运行中拒绝（避免删掉正在写入的日志）。
    ///
    /// **磁盘删除由调用方负责**（`SessionCatalog::delete`）——本方法只负责内存编排：
    /// 从 `sessions` 移除该会话；若删掉的正是前台会话，则**新建一份惰性空会话并切过去**
    /// （与启动时同一 UX：此刻只定名、不落盘，发首条消息才建文件）。这样删掉当前会话后
    /// 前台指针不会悬空，`active_name()` 也不会再返回已删的名字。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn delete_session(&mut self, name: &str) -> Result<(), String> {
        let id = self
            .id_of_name(name)
            .ok_or_else(|| format!("会话不存在：{name}"))?;
        if self
            .sessions
            .get(&id)
            .map(|session| session.running)
            .unwrap_or(false)
        {
            return Err("agent 正在运行中".to_owned());
        }
        let was_active = id == self.active;
        if was_active {
            // 删的是前台会话：**先**补一份空会话并切过去（`create_new` 会把 `active`
            // 挪到新会话），再移除旧的——顺序反了的话，一旦 `create_new` 失败就会
            // 留下悬空的 `active`（后续 `active()` 直接 panic）。
            self.create_new(None)?;
        }
        self.sessions.remove(&id);
        Ok(())
    }

    // ---- 按会话名的运行时访问（desktop 的 Tauri command 驱动；TUI 走 Deref 访问 active）----
    //
    // 关键点：全部**按会话名**取，不依赖 `active` 指针——后台会话运行时前台可能已经
    // 切走，按 `active` 取会把事件 / Agent 错记到别的会话（串会话 bug 的根因）。

    /// 名为 `name` 的会话（只读）。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn session(&self, name: &str) -> Option<&Session> {
        self.id_of_name(name).and_then(|id| self.sessions.get(&id))
    }

    /// 名为 `name` 的会话（可变）。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn session_mut(&mut self, name: &str) -> Option<&mut Session> {
        self.id_of_name(name)
            .and_then(|id| self.sessions.get_mut(&id))
    }

    /// 订阅名为 `name` 会话的事件流。会话不存在返回 `None`。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn subscribe(&self, name: &str) -> Option<broadcast::Receiver<Result<AgentEvent, String>>> {
        self.session(name).map(Session::subscribe)
    }

    /// 名为 `name` 会话的视图快照（会话不存在返回 `None`）。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn snapshot(&self, name: &str) -> Option<Vec<ItemWire>> {
        self.session(name).map(Session::snapshot)
    }

    /// 发起一轮：把名为 `name` 会话的 `Agent` 移出，并把**用户消息**乐观记入该会话的
    /// 视图条目（与 TUI 的 `App::submit` 对称）。
    ///
    /// 为什么要在这里记：`Session::apply_event` **有意忽略** `MessageAdded(User)`
    /// （见其注释）——用户消息由**驱动方**记录，而非事件流。TUI 在 `submit` 里记了，
    /// desktop 的 `agent_send` 之前漏了，导致 `Session.items` 永不含用户消息，
    /// 切走再切回时按快照重建就丢了用户消息（本函数的回归测试覆盖此点）。
    ///
    /// 会话不存在、或正在运行返回 `None`。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn begin_turn(&mut self, name: &str, user_message: String) -> Option<Agent> {
        let session = self.session_mut(name)?;
        let agent = session.agent.take()?;
        session.running = true;
        session.push_message(Role::User, user_message, false);
        Some(agent)
    }

    /// 把 `Agent` 放回名为 `name` 的会话。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn restore_agent(&mut self, name: &str, agent: Agent) {
        if let Some(session) = self.session_mut(name) {
            session.agent = Some(agent);
            session.running = false;
        }
    }

    /// 把一条事件累加进名为 `name` 的会话，并扇出给其订阅者。
    ///
    /// 运行期由驱动 `run_stream` 的任务调用（`SessionManager` 作为任务执行者）。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn apply_event(&mut self, name: &str, event: Result<AgentEvent, String>) {
        if let Some(session) = self.session_mut(name) {
            session.apply_event(event.clone());
            session.publish(event);
        }
    }

    /// 会话"结束"时触发一次增量 curation（`docs/memory.md` §5.1）。
    ///
    /// V1 的"结束"判定：**离开前台**（切换到别的会话 / 新建会话 / 删掉前台会话）。
    /// 这是一个**近似**——用户可能只是临时切走又切回（记为已知缺口，docs/memory.md
    /// §11.4：退出 / 空闲超时等更精确的判定留待后续）。触发是 best-effort：没有
    /// 记忆、没有消息、不在 tokio 运行时（单测）时静默跳过。
    ///
    /// 用 `Handle::try_current()` 守卫：curation 是后台任务，**绝不阻塞**会话切换
    /// 这条同步路径。失败只记一条 stderr 日志，不影响主流程。
    fn spawn_curation(&self, name: &str) {
        let Some(inputs) = self.curation_inputs(name) else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(Self::run_curation(inputs));
    }

    /// 退出程序前对**前台会话**跑一次 curation 并**等待完成**。
    ///
    /// 与 [`Self::spawn_curation`]（会话切换时后台触发）互补：单会话聊到底、直接退出
    /// 时，切换路径永远不会发生，curation 也就永远不触发（`docs/memory.md` §5.1 已知
    /// 缺口）。退出是「本轮结束」最确切的判定点，故在此补一次。用 `await` 而非
    /// `spawn`：主循环退出后 tokio 运行时随即关闭，`spawn` 出去的任务可能来不及跑完
    /// 就被丢弃——退出路径上必须同步等它落地。
    pub async fn finish(&self) {
        if let Some(name) = self.active().name.clone()
            && let Some(inputs) = self.curation_inputs(&name)
        {
            Self::run_curation(inputs).await;
        }
    }

    /// 收集一次 curation 所需的全部输入；不满足条件（无工厂 / 无记忆 / 无消息）返回
    /// `None`。抽出来是为了让 [`Self::spawn_curation`] 与 [`Self::finish`] 共用同一份
    /// 前置判定与快照逻辑。
    fn curation_inputs(&self, name: &str) -> Option<CurationInputs> {
        let factory = self.factory.clone()?;
        let session = self.session(name)?;
        // 只沉淀"有内容"的会话（用户 + 助手轮次）；空 / 单条不做无谓的模型调用。
        let messages: Vec<Message> = session
            .agent
            .as_ref()
            .map(|a| a.messages().to_vec())
            .unwrap_or_default();
        if messages.is_empty() {
            return None;
        }
        Some(CurationInputs {
            factory,
            // 无记忆配置的会话不沉淀（无 store 可写）。
            memory: session.memory.clone()?,
            messages,
            session_ref: format!("{name}.jsonl"),
        })
    }

    /// 一次 curation 的完整流水线：增量抽取 → 命中计数回写 → 定期整理 → 向量刷新。
    ///
    /// 全程 best-effort：失败只记一条 stderr，绝不向上传播（curation 是后台副业，
    /// 不该影响会话本身）。
    async fn run_curation(inputs: CurationInputs) {
        let CurationInputs {
            factory,
            memory,
            messages,
            session_ref,
        } = inputs;

        match factory.curate(&messages, &session_ref).await {
            Ok(outcome) if !outcome.empty || !outcome.written.is_empty() => {
                eprintln!(
                    "[memory] curated `{session_ref}`: {} written, {} rejected",
                    outcome.written.len(),
                    outcome.rejected.len()
                );
            }
            Ok(_) => {}
            Err(error) => eprintln!("[memory] curation for `{session_ref}` failed: {error}"),
        }
        // V2-D：把本轮会话攒下的检索命中次数写回条目（usage_count / utility）。
        match memory.flush_usage() {
            Ok(0) => {}
            Ok(n) => eprintln!("[memory] flushed usage for {n} entr(y/ies)"),
            Err(error) => eprintln!("[memory] usage flush failed: {error}"),
        }
        // V2-B/C：增量 curation 之后再按阈值触发定期整理（睡眠学习）。
        if factory.should_consolidate() {
            match factory.consolidate().await {
                Ok(outcome) if !outcome.empty => eprintln!(
                    "[memory] consolidated: {} merged, {} superseded, {} requalified, {} rejected",
                    outcome.merged,
                    outcome.superseded,
                    outcome.requalified,
                    outcome.rejected.len()
                ),
                Ok(_) => {}
                Err(error) => eprintln!("[memory] consolidation failed: {error}"),
            }
        }
        // V2.5：增量 / 整理都落盘后，把新条目嵌入、清理孤儿向量，让下一轮混合检索
        // 的语义腿立刻用得上（未配置 embedding 时空操作）。
        match memory.refresh_vectors().await {
            Ok(0) => {}
            Ok(n) => eprintln!("[memory] embedded {n} entr(y/ies) into the vector index"),
            Err(error) => eprintln!("[memory] vector refresh failed: {error}"),
        }
    }

    /// 名为 `name` 的**已加载**会话是否正在运行（`Agent` 被移入后台任务）。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn is_running(&self, name: &str) -> bool {
        self.session(name).map(|session| session.running).unwrap_or(false)
    }

    /// 前台会话的 `Agent`（只读）。运行中（被移入后台任务）返回 `None`。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn active_agent(&self) -> Option<&Agent> {
        self.active().agent.as_ref()
    }

    /// 前台会话的 `Agent`（可变）。运行中返回 `None`。
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn active_agent_mut(&mut self) -> Option<&mut Agent> {
        self.active_mut().agent.as_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::EmptySessionCatalog;
    use shirley_agent_sdk::{ModelConfig, ModelProtocol};

    fn test_agent() -> Agent {
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        Agent::builder().model_config(config).build().unwrap()
    }

    fn manager() -> SessionManager {
        SessionManager::single(
            test_agent(),
            Some("s".into()),
            Arc::new(EmptySessionCatalog),
            None,
            None,
        )
    }

    /// 回归：desktop 切换会话后切回来，用户消息不能丢。
    ///
    /// 快照（切回时前端据此重建 transcript）来自 `Session.items`；`apply_event`
    /// 有意忽略 `MessageAdded(User)`，故用户消息必须由驱动方在发起一轮时记入——
    /// `begin_turn` 就是干这个的。此前 desktop 的 `agent_send` 没记，快照里没有
    /// 用户消息，切换回来就"消失"了。
    #[test]
    fn begin_turn_records_user_message_into_snapshot() {
        let mut manager = manager();
        let agent = manager.begin_turn("s", "hello".into());
        assert!(agent.is_some(), "首轮应能取到 Agent");
        assert!(manager.is_running("s"), "取走 Agent 后会话应标记为运行中");

        let snapshot = manager.snapshot("s").unwrap();
        assert_eq!(snapshot.len(), 1, "快照应含刚记入的用户消息");
        match &snapshot[0] {
            ItemWire::Message { role, content, .. } => {
                assert_eq!(*role, Role::User);
                assert_eq!(content, "hello");
            }
            other => panic!("期望 Message 条目，实得 {other:?}"),
        }

        // 同一会话运行中不得再次发起（不并发驱动同一 Agent）。
        assert!(manager.begin_turn("s", "again".into()).is_none());
    }
}
