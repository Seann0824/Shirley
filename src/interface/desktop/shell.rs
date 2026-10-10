//! Tauri 2 壳：把 `AgentFactory` 造出的多会话 `Agent` 接进 webview。
//!
//! 事件契约（与 `web/src/lib/bridge.ts` 对齐）：
//! - command `agent_send { text, references }`：发起**当前前台会话**的一轮 `run_stream`，
//!   事件经 `agent://event` 推送并带来源会话名。`references` 是 `@` 引用的工作区路径，
//!   会拼进本轮 prompt。
//! - command `agent_cancel`：预留（`StopReason::Cancelled` 尚未产生，见 `Agent.md` 缺口 9）。
//! - command `agent_model_name`：页脚 / 选择器展示当前模型名。
//! - command `agent_list_models`：列出可选模型（`ModelCatalog`）。
//! - command `agent_set_model`：热切换模型（`Agent::set_model`，不重建 Agent）。
//! - command `agent_search_files { query }`：`@` 引用的工作区文件检索（应用层，不经 SDK）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use serde::Serialize;
use shirley_agent_sdk::{Agent, Message};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

use crate::interface::session::SessionManager;
use super::wire::{AgentEventWire, SessionSnapshotWire};
use crate::bootstrap::AgentFactory;
use crate::models::ModelCatalog;
use crate::session::{
    SessionCatalog, SessionEntry, first_user_message, sanitize_title, title_prompt,
};

/// webview 侧监听的统一事件名。
const EVENT_NAME: &str = "agent://event";

/// Tauri 管理的运行时状态。
///
/// 多会话编排收在 [`SessionManager`] 里（`docs/multi-session.md` 决策 4）：
/// 每个会话自持一个独立 `Agent`，`active` 指向前台。一轮运行期间把**该会话**的
/// `Agent` `take` 出去移入后台任务，结束后放回——同一会话不会并发驱动，
/// 不同会话可并行（与 TUI 共用同一套 `SessionManager` 语义）。
struct DesktopState {
    sessions: Arc<Mutex<SessionManager>>,
    model_catalog: Arc<dyn ModelCatalog>,
    /// 会话目录（新建 / 重命名 / 删除用）——与 TUI **共用同一个实现**，
    /// 因此两边读的是同一份 `<root>/.shirley/sessions` 数据。
    session_catalog: Arc<dyn SessionCatalog>,
    /// 工作区根目录：`@` 文件检索用（应用层 `workspace_search`，不碰 SDK）。
    working_dir: PathBuf,
    /// 每个会话的事件 pump 任务句柄（订阅该会话 `broadcast` → 转线格式 → emit）。
    /// 前端 `agent_subscribe` 时插入，`agent_unsubscribe` / 切走时 abort 并移除。
    pumps: Arc<Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>>,
}

/// 前端消费的模型条目（与 `web/src/lib/bridge.ts` 的 `ModelEntry` 对齐）。
#[derive(Debug, Clone, Serialize)]
struct ModelEntryWire {
    label: String,
    value: String,
    provider: String,
}

/// `@` 引用可选的条目（与 `web/src/lib/bridge.ts` 的 `FileEntry` 对齐）。
#[derive(Debug, Clone, Serialize)]
struct FileEntryWire {
    path: String,
    name: String,
    kind: &'static str,
}

/// 前端消费的会话条目（与 `web/src/lib/bridge.ts` 的 `SessionEntry` 对齐）。
///
/// `label` 是展示用标题（自定义标题优先，否则回落 `name`）；`preview` 是首条
/// 用户消息；`modified_ms` / `turns` 对齐 Codex resume picker 的时间与轮数。
#[derive(Debug, Clone, Serialize)]
struct SessionEntryWire {
    name: String,
    label: String,
    preview: String,
    modified_ms: u64,
    turns: usize,
    /// 该会话当前是否在跑一轮（后台活跃）——列表据此显示 loading 转圈。
    /// 由 [`agent_list_sessions`] 从 `SessionManager` 内存态合并（磁盘目录不知道运行态）。
    running: bool,
}

impl From<SessionEntry> for SessionEntryWire {
    fn from(entry: SessionEntry) -> Self {
        Self {
            name: entry.name,
            label: entry.label,
            preview: entry.preview,
            modified_ms: entry.modified_ms,
            turns: entry.turns,
            // 目录层不感知运行态，默认 `false`；真正的运行态由 command 合并。
            running: false,
        }
    }
}

/// 恢复会话时回放给前端的一条历史消息（与 `web/src/lib/bridge.ts` 的
/// `HistoryMessage` 对齐）。只透出可展示的 User / Assistant 正文——
/// system / tool / context_summary 不是"对话"，不展示（与 TUI 展示口径一致）。
#[derive(Debug, Clone, Serialize)]
struct HistoryMessageWire {
    role: &'static str,
    content: String,
}

/// 从 `Agent` 当前消息表提取可展示的历史（跳 system / tool / context_summary）。
fn history_from_messages(messages: &[Message]) -> Vec<HistoryMessageWire> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(HistoryMessageWire {
                role: "user",
                content: content.clone(),
            }),
            Message::Assistant { content, .. } => content
                .as_ref()
                .filter(|text| !text.trim().is_empty())
                .map(|text| HistoryMessageWire {
                    role: "assistant",
                    content: text.clone(),
                }),
            _ => None,
        })
        .collect()
}

/// 把消息正文与 `@` 引用拼成这一轮发给 Agent 的 prompt。
///
/// 引用只是**这一轮输入的一部分**，不进入 SDK 的对外契约（`Message` / `AgentEvent`
/// 都不动）。用 markdown 风格的行内引用标注，让模型知道用户点了哪些文件/目录。
fn compose_prompt(text: &str, references: &[String]) -> String {
    if references.is_empty() {
        return text.to_owned();
    }
    let mut prompt = String::new();
    prompt.push_str("引用的工作区文件/目录：\n");
    for reference in references {
        prompt.push_str("- ");
        prompt.push_str(reference);
        prompt.push('\n');
    }
    prompt.push('\n');
    prompt.push_str(text);
    prompt
}

#[tauri::command]
async fn agent_send(
    state: State<'_, DesktopState>,
    text: String,
    references: Option<Vec<String>>,
    session: Option<String>,
) -> Result<(), String> {
    // 目标会话：显式传入则用它，否则回落到前台会话。**按会话名**取 Agent，
    // 不依赖 `active` 指针——后台会话运行时前台可能已切走（修串会话 bug）。
    let session_name = {
        let guard = state.sessions.lock().await;
        match session {
            Some(name) => name,
            None => guard
                .active_name()
                .map(str::to_owned)
                .ok_or_else(|| "当前会话尚未落盘".to_owned())?,
        }
    };
    let prompt = compose_prompt(&text, references.as_deref().unwrap_or(&[]));

    // 发起本轮：取走该会话的 `Agent`，并把**用户消息**乐观记入其视图条目。
    // 必须记：`Session::apply_event` 有意忽略 `MessageAdded(User)`（用户消息由驱动方
    // 记录，与 TUI 的 `App::submit` 对称），不记则 `Session.items` 永不含用户消息，
    // 切走再切回按快照重建时用户消息就丢了。记的是**人类可读原文**（`text`），
    // 不是拼了引用头的 `prompt`。
    let (mut agent, memory) = {
        let mut guard = state.sessions.lock().await;
        let agent = guard
            .begin_turn(&session_name, text.clone())
            .ok_or_else(|| "agent 正在运行中".to_owned())?;
        // 记忆：把本轮用户输入作为 query，供 provider 组装请求时做相关检索注入
        // （core.md 常驻 + top-k 相关，V2.5 混合检索）。query 向量在下面的任务里
        // 预取（embedding 是异步 HTTP，而 provider 的 `context()` 是同步的）。
        let memory = guard
            .session(&session_name)
            .and_then(|session| session.memory.clone());
        if let Some(memory) = memory.as_ref() {
            memory.set_query(text.as_str());
        }
        (agent, memory)
    };

    // `SessionManager` 作为任务执行者：跑 `run_stream`，把每个事件喂回**该会话**
    // （累加 + 扇出给订阅者）。前端经 pump（`agent_subscribe`）消费扇出。
    let sessions = state.sessions.clone();
    // 自动命名需要目录句柄（写标题 sidecar），克隆一份进任务。
    let title_catalog = state.session_catalog.clone();
    tauri::async_runtime::spawn(async move {
        // 记忆 V2.5：预取本轮 query 向量（未配置 embedding / 失败时是空操作，相关腿
        // 退化为纯 BM25，绝不打断对话）。
        if let Some(memory) = memory.as_ref() {
            let _ = memory.prefetch_query_vector().await;
        }
        {
            // `stream` 借用了 `agent`，用块把它圈住，出了块再 move `agent` 放回。
            let mut stream = agent.run_stream(&prompt);
            while let Some(event) = stream.next().await {
                let update = match event {
                    Ok(event) => Ok(event),
                    Err(error) => Err(error.to_string()),
                };
                sessions.lock().await.apply_event(&session_name, update);
            }
        }
        // 会话自动命名：首轮结束后，若该会话还没有自定义标题，就用首条用户消息
        // 让模型生成一个短标题写进 sidecar。失败 / 已有标题都静默跳过（下轮再试）。
        // 必须在 `restore_agent` 之前——此刻 `agent` 还在本地，可只读调 `complete`。
        auto_title_session(&agent, &title_catalog, &session_name).await;
        // 无论成败都把 Agent 放回该会话，供下一轮复用。
        sessions.lock().await.restore_agent(&session_name, agent);
    });

    Ok(())
}

/// 首轮结束后为会话自动生成标题（AI 命名）。
///
/// 触发条件：该会话**还没有自定义标题**（目录里 `label == name`，即从未被命名 /
/// 重命名过）。用户手动改过名后 `label != name`，这里就不再覆盖——"自动"只做一次，
/// 尊重用户意图。
///
/// 标题取自该会话的**首条用户消息**（[`first_user_message`]），经一次无工具、
/// 非流式的补全（[`Agent::complete`]）生成，再用 [`sanitize_title`] 清洗后经
/// `SessionCatalog::rename` 写入 `<name>.title` sidecar。
///
/// 任何一步失败都**静默忽略**：命名是锦上添花，不该让一次聊天失败，也留待下一轮
/// 再试（尚未命名的会话每轮都会走到这里）。
async fn auto_title_session(agent: &Agent, catalog: &Arc<dyn SessionCatalog>, name: &str) {
    // 目录扫描是同步 IO，移到阻塞线程池。
    let probe_catalog = catalog.clone();
    let probe_name = name.to_owned();
    let already_named = tauri::async_runtime::spawn_blocking(move || {
        probe_catalog
            .list()
            .ok()
            .and_then(|entries| entries.into_iter().find(|entry| entry.name == probe_name))
            .map(|entry| entry.label != entry.name)
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if already_named {
        return;
    }

    let Some(first) = first_user_message(agent.messages()) else {
        return;
    };
    let Ok(raw) = agent.complete(&title_prompt(&first)).await else {
        return;
    };
    let Some(title) = sanitize_title(&raw) else {
        return;
    };

    let write_catalog = catalog.clone();
    let write_name = name.to_owned();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        write_catalog.rename(&write_name, &title)
    })
    .await;
}

/// 订阅一个会话：返回其**视图快照**（供前端重建 transcript），并起一个 per-session
/// pump 把该会话后续事件转成线格式 emit 给 webview。
///
/// 幂等：重复订阅会先停掉旧 pump 再起新的。切到某会话时调它，切走时调
/// `agent_unsubscribe`。
#[tauri::command]
async fn agent_subscribe(
    state: State<'_, DesktopState>,
    app: AppHandle,
    session: String,
) -> Result<SessionSnapshotWire, String> {
    // 先停掉该会话已有的 pump（幂等重订阅）。
    if let Some(handle) = state.pumps.lock().await.remove(&session) {
        handle.abort();
    }
    // **同一把锁内**先订阅再取快照：`apply_event` 也要拿这把锁，故两者之间不会
    // 插入任何事件——快照覆盖订阅前、通道覆盖订阅后，既不丢也不重。
    let (mut rx, items, running) = {
        let guard = state.sessions.lock().await;
        let rx = guard
            .subscribe(&session)
            .ok_or_else(|| format!("会话不存在：{session}"))?;
        let items = guard
            .snapshot(&session)
            .ok_or_else(|| format!("会话不存在：{session}"))?;
        let running = guard.is_running(&session);
        (rx, items, running)
    };

    let pumps = state.pumps.clone();
    let pump_session = session.clone();
    let handle = tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Ok(event)) => {
                    let wire = AgentEventWire::from_event(event).with_session(&pump_session);
                    let _ = app.emit(EVENT_NAME, wire);
                }
                Ok(Err(message)) => {
                    let wire = AgentEventWire::Error {
                        session: Some(pump_session.clone()),
                        message,
                    };
                    let _ = app.emit(EVENT_NAME, wire);
                }
                // 订阅者落后于扇出：跳过落后的事件，前端靠下次订阅快照重对齐。
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                // 所有发送者已丢弃（应用关停）。
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        // 通道关闭：自我清理（若已被替换，同名新 pump 会被误删——但仅在关停时发生）。
        pumps.lock().await.remove(&pump_session);
    });
    let snapshot = SessionSnapshotWire::new(session.clone(), items, running);
    state.pumps.lock().await.insert(session, handle);

    Ok(snapshot)
}

/// 停掉一个会话的 pump（切走时调用）。不存在的会话静默成功（幂等）。
#[tauri::command]
async fn agent_unsubscribe(state: State<'_, DesktopState>, session: String) -> Result<(), String> {
    if let Some(handle) = state.pumps.lock().await.remove(&session) {
        handle.abort();
    }
    Ok(())
}

#[tauri::command]
async fn agent_model_name(state: State<'_, DesktopState>) -> Result<String, String> {
    Ok(state
        .sessions
        .lock()
        .await
        .active_agent()
        .map(|agent| agent.model_config().model.clone())
        .unwrap_or_default())
}

/// 列出可选模型（模型目录由 `AgentFactory` 注入）。
#[tauri::command]
async fn agent_list_models(state: State<'_, DesktopState>) -> Result<Vec<ModelEntryWire>, String> {
    let entries = state.model_catalog.list().await;
    Ok(entries
        .into_iter()
        .map(|entry| ModelEntryWire {
            label: entry.label,
            value: entry.value,
            provider: entry.provider,
        })
        .collect())
}

/// 热切换模型：不重建 Agent，只换 `ModelConfig::model`（与 TUI 的 `/model` 同语义）。
#[tauri::command]
async fn agent_set_model(state: State<'_, DesktopState>, model: String) -> Result<(), String> {
    let mut guard = state.sessions.lock().await;
    let agent = guard
        .active_agent_mut()
        .ok_or_else(|| "agent 正在运行中".to_owned())?;
    agent.set_model(model);
    Ok(())
}

/// `@` 引用的工作区文件检索。纯应用层（`workspace_search`），SDK 不参与。
#[tauri::command]
async fn agent_search_files(
    state: State<'_, DesktopState>,
    query: String,
) -> Result<Vec<FileEntryWire>, String> {
    let root = state.working_dir.clone();
    // 遍历是同步 IO，移到阻塞线程池，别卡住 async runtime。
    let entries = tauri::async_runtime::spawn_blocking(move || {
        crate::workspace_search::search_files(&root, &query, None)
    })
    .await
    .map_err(|error| error.to_string())?;
    Ok(entries
        .into_iter()
        .map(|entry| FileEntryWire {
            path: entry.path,
            name: entry.name,
            kind: match entry.kind {
                crate::workspace_search::FileKind::Dir => "dir",
                crate::workspace_search::FileKind::File => "file",
            },
        })
        .collect())
}

/// 列出全部会话（按最近修改降序）。与 TUI 的 `/session` 读同一份目录。
#[tauri::command]
async fn agent_list_sessions(
    state: State<'_, DesktopState>,
) -> Result<Vec<SessionEntryWire>, String> {
    let catalog = state.session_catalog.clone();
    // 目录扫描是同步 IO，移到阻塞线程池，别卡住 async runtime。
    let entries = tauri::async_runtime::spawn_blocking(move || catalog.list())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    // 目录扫描只给磁盘元数据；**运行态在 `SessionManager` 内存里**，在这里合并——
    // 否则后台活跃的会话在列表里看不出「正在跑」（前端据此转圈）。
    let sessions = state.sessions.lock().await;
    Ok(entries
        .into_iter()
        .map(|entry| {
            let running = sessions.is_running(&entry.name);
            let mut wire = SessionEntryWire::from(entry);
            wire.running = running;
            wire
        })
        .collect())
}

/// 当前会话名（供前端展示 / 打标）。
#[tauri::command]
async fn agent_current_session(state: State<'_, DesktopState>) -> Result<Option<String>, String> {
    Ok(state
        .sessions
        .lock()
        .await
        .active_name()
        .map(str::to_owned))
}

/// 新建会话并可命名（`title` 为空 = 匿名，回落到首条用户消息 / 名字）。
///
/// 经 `SessionManager` 惰性新建并切过去——与 TUI 选择器「＋ 新建会话」同语义。
#[tauri::command]
async fn agent_new_session(
    state: State<'_, DesktopState>,
    title: Option<String>,
) -> Result<SessionEntryWire, String> {
    // 目录扫描 / 建目录是同步 IO，移到阻塞线程池。
    let catalog = state.session_catalog.clone();
    let title_for_list = title.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        // 预先探一次目录（保证惰性新建前目录已就绪）；真正的建名在 manager 里。
        let _ = catalog.list();
        title_for_list
    })
    .await;

    let name = state
        .sessions
        .lock()
        .await
        .create_new(title.as_deref())?;
    // 回读目录条目以拿到 label / preview（惰性新建时大多为空）。
    let entry = SessionEntryWire {
        name: name.clone(),
        label: name.clone(),
        preview: String::new(),
        modified_ms: 0,
        turns: 0,
        // 刚建的空会话尚未运行。
        running: false,
    };
    Ok(entry)
}

/// 切换到指定会话：经 `SessionManager` 挪 `active` 指针（与 TUI 同一接缝）。
///
/// 已加载会话 → 直接复用其就绪 `Agent`（不重建，保住后台上下文）；
/// 未加载 → 从目录 `open` 日志、经工厂恢复工作集后切前台。
#[tauri::command]
async fn agent_switch_session(
    state: State<'_, DesktopState>,
    name: String,
) -> Result<(), String> {
    state.sessions.lock().await.switch_to(&name)?;
    Ok(())
}

/// 重命名会话：**只改标题，不改标识 / 内容**（Codex 的 `/rename` 语义）。
#[tauri::command]
async fn agent_rename_session(
    state: State<'_, DesktopState>,
    name: String,
    title: String,
) -> Result<(), String> {
    let catalog = state.session_catalog.clone();
    tauri::async_runtime::spawn_blocking(move || catalog.rename(&name, &title))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// 删除会话（连同日志与标题）。运行中拒绝，避免删掉正在写入的日志。
///
/// 先经 `SessionManager::delete_session` 做**内存编排**：移出该会话；若删掉的正是前台
/// 会话，则补一份惰性空会话并切过去（否则 `active` 会悬空，`currentSession` 仍回被删
/// 的名字）。磁盘删除随后交给 `SessionCatalog`。
#[tauri::command]
async fn agent_delete_session(
    state: State<'_, DesktopState>,
    name: String,
) -> Result<(), String> {
    state.sessions.lock().await.delete_session(&name)?;
    let catalog = state.session_catalog.clone();
    let delete_name = name.clone();
    tauri::async_runtime::spawn_blocking(move || catalog.delete(&delete_name))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// 回放当前会话历史（切换 / 启动后重建 transcript 用）。
#[tauri::command]
async fn agent_load_history(
    state: State<'_, DesktopState>,
) -> Result<Vec<HistoryMessageWire>, String> {
    let guard = state.sessions.lock().await;
    let agent = guard
        .active_agent()
        .ok_or_else(|| "agent 正在运行中".to_owned())?;
    Ok(history_from_messages(agent.messages()))
}

/// 预留：取消本轮运行（SDK 侧取消机制尚未落地，见 `Agent.md` 缺口 9）。
#[tauri::command]
fn agent_cancel() {}

pub fn run(factory: AgentFactory) -> std::io::Result<()> {
    // 启动时惰性开一份空会话（此刻只定名、不落盘，发首条消息才建文件），
    // 与 TUI 的启动 UX 一致。
    let session_catalog = factory.session_catalog.clone();
    let model_catalog = factory.model_catalog.clone();
    let working_dir = factory.working_dir.clone();
    let (entry, store) = session_catalog
        .create_lazy(None)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    // 会话持久化在应用层：load 得空工作集交给工厂起 `Agent`，store 一并交给
    // `SessionManager`——落库由其事件驱动路径完成。
    let log = store
        .load()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let built = factory
        .build_agent(log)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    eprintln!("[desktop] 会话：{}", entry.name);

    let sessions = SessionManager::with_factory(
        built.agent,
        Some(entry.name),
        session_catalog.clone(),
        Arc::new(factory),
        Some(store),
        Some(built.memory),
    );

    let state = DesktopState {
        sessions: Arc::new(Mutex::new(sessions)),
        model_catalog,
        session_catalog,
        working_dir,
        pumps: Arc::new(Mutex::new(HashMap::new())),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            agent_send,
            agent_subscribe,
            agent_unsubscribe,
            agent_cancel,
            agent_model_name,
            agent_list_models,
            agent_set_model,
            agent_search_files,
            agent_list_sessions,
            agent_current_session,
            agent_new_session,
            agent_switch_session,
            agent_rename_session,
            agent_delete_session,
            agent_load_history
        ])
        .run(tauri::generate_context!())
        .map_err(|error| std::io::Error::other(error.to_string()))
}
