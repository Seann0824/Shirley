//! Tauri 2 壳：把 `AgentFactory` 造出的多会话 `Agent` 接进 webview。
//!
//! # 多项目（多工作区）
//!
//! Shirley 作为独立 app 不绑定单一目录：用户可把多个工作区登记为「项目」，每个
//! 项目下挂自己的会话。注册表（[`ProjectRegistry`]）只记「项目在哪」，落
//! `<config_dir>/shirley/projects.json`；**每个项目一个 [`AgentFactory`]**（各自
//! 读该项目 `<root>/.shirley/config.toml`），会话日志仍落各项目自己的
//! `<root>/.shirley/sessions/`（随仓库走）。
//!
//! 「移除项目」= **归档**：只置 `archived` 标志，不删磁盘数据，可从归档列表恢复。
//!
//! # 事件契约（与 `web/src/lib/bridge.ts` 对齐）
//!
//! - command `agent_send { project, session?, text, references? }`：发起指定会话的一轮
//!   `run_stream`，事件经 `agent://event` 推送并带来源 `project` + `session`。
//! - command `agent_subscribe { project, session }`：订阅一个会话（返回快照 + 起 pump）。
//! - command `agent_list_projects { archived? }` / `agent_add_project`（原生文件夹选择器）
//!   / `agent_rename_project` / `agent_archive_project` / `agent_restore_project`。
//! - command `agent_cancel { project, session }`：取消指定会话正在跑的一轮——向驱动
//!   该会话 `run_stream` 的任务发取消信号，任务 drop 掉流、回退 Agent 状态并以
//!   `Finished(Cancelled)` 收尾。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use serde::Serialize;
use shirley_agent_sdk::{Agent, AgentEvent, Message, RunResult, StopReason};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::{Mutex, oneshot};

use super::wire::{AgentEventWire, SessionSnapshotWire, TaskViewWire};
use crate::bootstrap::AgentFactory;
use crate::interface::session::SessionManager;
use crate::project::{ProjectEntry, ProjectRegistry};
use crate::session::{
    SessionCatalog, SessionEntry, first_user_message, sanitize_title, title_prompt,
};

/// webview 侧监听的统一事件名。
const EVENT_NAME: &str = "agent://event";

/// 单个项目的运行时：一份装配好的工厂 + 它的多会话编排器。
///
/// 会话日志目录 / 系统提示词 / 记忆根都随 `factory`（即该项目的 `working_dir`）走；
/// 因此不同项目天然隔离，互不共享可变状态。
struct ProjectRuntime {
    factory: Arc<AgentFactory>,
    sessions: SessionManager,
}

/// Tauri 管理的运行时状态（多项目）。
///
/// - `registry`：项目注册表（哪些目录是项目）。
/// - `runtimes`：**已打开**项目的运行时，按项目 id 懒加载（`ensure_runtime`）。
///   关掉 / 归档项目即从表里移除，其内存态会话随之释放。
/// - `pumps`：每个「项目::会话」的事件 pump 句柄。
struct DesktopState {
    registry: Arc<Mutex<ProjectRegistry>>,
    runtimes: Arc<Mutex<HashMap<String, Arc<Mutex<ProjectRuntime>>>>>,
    pumps: Arc<Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>>,
    /// 每个「项目::会话」正在运行一轮时的取消句柄。`agent_send` 发起一轮时插入，
    /// 结束（正常 / 取消 / 出错）时移除；`agent_cancel` 取出并 `send(())` 触发打断。
    /// 键与 `pumps` 一致（`route_key`）。
    cancels: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}

/// 事件 pump 的键：跨项目不重名（会话名在项目内唯一，项目 id 全局唯一）。
fn route_key(project: &str, session: &str) -> String {
    format!("{project}::{session}")
}

/// 用一份现成工厂造一个项目运行时。
///
/// **恢复语义**：优先打开该项目**最近修改**的会话（`SessionCatalog::list` 已按
/// `modified_ms` 降序），把它作为前台会话恢复；项目**没有任何会话**时才惰性开一份
/// 空会话（此刻只定名、不落盘）。
///
/// 这里必须"恢复最近会话"而不是无条件新建：`ensure_runtime` 在项目**首次懒加载**
/// 时就会调用本函数（前端切项目 / 侧栏轮询 `agent_list_sessions` 都会触发），若总是
/// `create_lazy(None)`，`agent_current_session` 就会把一个**刚凭空造出的空会话**报给
/// 前端，表现为"切项目把当前选中 session 切成 new session"。
fn runtime_from_factory(factory: Arc<AgentFactory>) -> Result<ProjectRuntime, String> {
    let catalog = factory.session_catalog.clone();
    // 最近修改的会话优先；没有会话才新建惰性空会话。
    let latest = catalog
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .next();
    let (name, store, from_disk) = match latest {
        Some(entry) => {
            let store = catalog.open(&entry.name).map_err(|e| e.to_string())?;
            (entry.name, store, true)
        }
        None => {
            let (entry, store) = catalog.create_lazy(None).map_err(|e| e.to_string())?;
            (entry.name, store, false)
        }
    };
    let log = store.load().map_err(|e| e.to_string())?;
    let built = factory.build_agent(log).map_err(|e| e.to_string())?;
    let mut sessions = SessionManager::with_factory(
        built.agent,
        Some(name),
        catalog,
        factory.clone(),
        Some(store),
        Some(built.memory),
        Some(built.skills),
        Some(built.todo),
    );
    // 从磁盘恢复的会话：其 `Agent` 已带全量历史，但视图条目还是空的——重建一次，
    // 快照 / 渲染才有内容（与 `SessionManager::switch_to` 的恢复路径一致）。
    if from_disk {
        sessions.active_mut().rebuild_items_from_agent();
    }
    Ok(ProjectRuntime { factory, sessions })
}

/// 对任意目录装配一份新工厂并造运行时（「新建项目」）。
fn build_runtime(root: PathBuf) -> Result<ProjectRuntime, String> {
    let factory = Arc::new(AgentFactory::assemble(root).map_err(|e| e.to_string())?);
    runtime_from_factory(factory)
}

/// 取（必要时懒加载）指定项目的运行时，并刷新其 `last_opened_ms`。
///
/// 未加载 → 查注册表拿到路径、`build_runtime` 后插入。项目不存在 / 目录已失效
/// 返回错误。
async fn ensure_runtime(
    state: &DesktopState,
    project: &str,
) -> Result<Arc<Mutex<ProjectRuntime>>, String> {
    if let Some(rt) = state.runtimes.lock().await.get(project).cloned() {
        state.registry.lock().await.touch(project);
        return Ok(rt);
    }
    let entry = state
        .registry
        .lock()
        .await
        .get(project)
        .cloned()
        .ok_or_else(|| format!("项目不存在：{project}"))?;
    let path = entry.path.clone();
    let runtime = tauri::async_runtime::spawn_blocking(move || build_runtime(path))
        .await
        .map_err(|e| e.to_string())??;
    let rt = {
        let mut map = state.runtimes.lock().await;
        map.entry(project.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(runtime)))
            .clone()
    };
    // 先释放 runtimes 锁再 touch（避免 registry/runtimes 嵌套加锁）。
    state.registry.lock().await.touch(project);
    Ok(rt)
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

/// `/` 技能候选条目（与 `web/src/lib/bridge.ts` 的 `SkillEntry` 对齐）。
#[derive(Debug, Clone, Serialize)]
struct SkillEntryWire {
    name: String,
    description: String,
}

/// 前端消费的项目条目（与 `web/src/lib/bridge.ts` 的 `ProjectEntry` 对齐）。
#[derive(Debug, Clone, Serialize)]
struct ProjectEntryWire {
    id: String,
    name: String,
    path: String,
    archived: bool,
    last_opened_ms: u64,
}

impl From<&ProjectEntry> for ProjectEntryWire {
    fn from(entry: &ProjectEntry) -> Self {
        Self {
            id: entry.id.clone(),
            name: entry.name.clone(),
            path: entry.path.to_string_lossy().into_owned(),
            archived: entry.archived,
            last_opened_ms: entry.last_opened_ms,
        }
    }
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
            running: false,
        }
    }
}

/// 恢复会话时回放给前端的一条历史消息（与 `web/src/lib/bridge.ts` 的
/// `HistoryMessage` 对齐）。只透出可展示的 User / Assistant 正文。
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

// ---------------------------------------------------------------------------
// 项目 commands
// ---------------------------------------------------------------------------

/// 列出项目（按最近打开降序）。`archived = true` 时包含归档项目（供归档列表）。
#[tauri::command]
async fn agent_list_projects(
    state: State<'_, DesktopState>,
    archived: Option<bool>,
) -> Result<Vec<ProjectEntryWire>, String> {
    let include_archived = archived.unwrap_or(false);
    let guard = state.registry.lock().await;
    Ok(guard
        .list(include_archived)
        .into_iter()
        .map(ProjectEntryWire::from)
        .collect())
}

/// 打开原生文件夹选择器新建项目；取消返回 `None`。
///
/// 已登记的目录按 id 去重：已归档则「恢复并改名」，未归档则只刷新打开时间——
/// 不会重复登记。
#[tauri::command]
async fn agent_add_project(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Option<ProjectEntryWire>, String> {
    // 原生对话框是阻塞调用，移到阻塞线程池（内部会切回主线程显示）。
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|file| file.into_path().ok()) else {
        return Ok(None);
    };

    let entry = state
        .registry
        .lock()
        .await
        .add(path, None)
        .map_err(|e| e.to_string())?;
    // 立即装配运行时（惰性空会话），让用户能马上在该项目下开聊。
    let root = entry.path.clone();
    let runtime = tauri::async_runtime::spawn_blocking(move || build_runtime(root))
        .await
        .map_err(|e| e.to_string())??;
    state
        .runtimes
        .lock()
        .await
        .insert(entry.id.clone(), Arc::new(Mutex::new(runtime)));
    Ok(Some(ProjectEntryWire::from(&entry)))
}

/// 重命名项目（只改展示名，不动路径 / 会话数据）。
#[tauri::command]
async fn agent_rename_project(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
) -> Result<(), String> {
    state
        .registry
        .lock()
        .await
        .rename(&project, &name)
        .map_err(|e| e.to_string())
}

/// 归档项目：只置标志、**不删磁盘数据**；同时释放其内存运行时。
#[tauri::command]
async fn agent_archive_project(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<(), String> {
    state
        .registry
        .lock()
        .await
        .archive(&project)
        .map_err(|e| e.to_string())?;
    // 归档即关闭：丢掉运行时（会话 Agent / 内存态一并释放，磁盘数据不动）。
    state.runtimes.lock().await.remove(&project);
    Ok(())
}

/// 从归档恢复项目（刷新最近打开时间；运行时下次打开时懒加载）。
#[tauri::command]
async fn agent_restore_project(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<(), String> {
    state
        .registry
        .lock()
        .await
        .restore(&project)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 会话 / 运行 commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn agent_send(
    state: State<'_, DesktopState>,
    project: String,
    text: String,
    references: Option<Vec<String>>,
    session: Option<String>,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let prompt = compose_prompt(&text, references.as_deref().unwrap_or(&[]));

    // 发起本轮：取走该会话的 `Agent`，并把**用户消息**乐观记入其视图条目
    // （`Session::apply_event` 有意忽略 `MessageAdded(User)`，由驱动方记录）。
    let (mut agent, memory, session_name) = {
        let mut guard = runtime.lock().await;
        let session_name = match session.clone() {
            Some(name) => name,
            None => guard
                .sessions
                .active_name()
                .map(str::to_owned)
                .ok_or_else(|| "当前会话尚未落盘".to_owned())?,
        };
        let agent = guard
            .sessions
            .begin_turn(&session_name, text.clone())
            .ok_or_else(|| "agent 正在运行中".to_owned())?;
        let memory = guard
            .sessions
            .session(&session_name)
            .and_then(|s| s.memory.clone());
        if let Some(memory) = memory.as_ref() {
            memory.set_query(text.as_str());
        }
        (agent, memory, session_name)
    };

    let catalog = runtime.lock().await.factory.session_catalog.clone();
    let runtime_for_task = runtime.clone();
    // 取消句柄：与本轮任务同生命周期。`agent_cancel` 取出它并发信号；任务结束
    // （正常 / 取消 / 出错）时移除，避免残留一个指向已结束任务的 sender。
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    let key = route_key(&project, &session_name);
    state.cancels.lock().await.insert(key.clone(), cancel_tx);
    let cancels = state.cancels.clone();

    tauri::async_runtime::spawn(async move {
        let mut cancel_rx = cancel_rx;
        let cancelled_before_start = if let Some(memory) = memory.as_ref() {
            tokio::select! {
                biased;
                _ = &mut cancel_rx => true,
                _ = memory.prefetch_query_vector() => false,
            }
        } else {
            false
        };

        // 驱动本轮：`tokio::select!` 在「流的下一个事件」与「取消信号」之间竞争。
        // 取消分支 `break` 后 `stream` 被 drop —— SDK 的 `run_stream` 是 async-stream
        // 生成的流，**drop 即停止**（与 TUI `run_agent` 同款手法）。没有这条路径时
        // 后台会一直把 ReAct 循环跑到底（点 stop 只改了前端 `busy`，从未真正打断）。
        let mut terminal_update = None;
        let mut cancelled = {
            let mut stream = agent.run_stream(&prompt);
            let mut cancelled = cancelled_before_start;
            while !cancelled {
                tokio::select! {
                    biased;
                    // `&mut` 借用：`select!` 每轮都会 poll 它，若按值移动会在第二轮报
                    // `use of moved value`（oneshot::Receiver 不 Copy）。
                    _ = &mut cancel_rx => {
                        cancelled = true;
                        break;
                    }
                    event = stream.next() => {
                        let Some(event) = event else { break };
                        let update = match event {
                            Ok(event) => Ok(event),
                            Err(error) => Err(error.to_string()),
                        };
                        if matches!(&update, Ok(AgentEvent::Finished(_)) | Err(_)) {
                            terminal_update = Some(update);
                            break;
                        }
                        runtime_for_task
                            .lock()
                            .await
                            .sessions
                            .apply_event(&session_name, update);
                    }
                }
            }
            cancelled
        };

        // 流已 drop：把半截助手回复真正写进 Agent 与日志，供下一轮参考。
        for message in agent.finish_interrupted_turn() {
            runtime_for_task
                .lock()
                .await
                .sessions
                .apply_event(&session_name, Ok(AgentEvent::MessageAdded(message)));
        }
        if !cancelled && matches!(&terminal_update, Some(Ok(AgentEvent::Finished(_)))) {
            // 标题生成也属于本轮等待，可随停止信号取消。
            cancelled = tokio::select! {
                biased;
                _ = &mut cancel_rx => true,
                _ = auto_title_session(&agent, &catalog, &session_name) => false,
            };
        }
        if cancelled {
            // 需求 2：打断**保留**本轮消息（用户消息 + 半截流式回复）作为下一轮参考，
            // 不再回退删除——与 TUI `App::complete_turn` 的打断语义对齐。
            // 因此这里**不** truncate agent / 不截尾日志。
            // 仍以 `Finished(Cancelled)` 收尾：让前端把本轮 assistant 标记为完成、
            // 清掉 `busy`，并让订阅者知道本轮真的结束了。
            terminal_update = Some(Ok(AgentEvent::Finished(RunResult {
                messages: Vec::new(),
                stop_reason: StopReason::Cancelled,
                usage: Default::default(),
            })));
        }

        // 收尾事件意味着可以立即发下一轮，因此先归还 Agent，再解锁前端。
        cancels.lock().await.remove(&key);
        let mut runtime = runtime_for_task.lock().await;
        runtime.sessions.restore_agent(&session_name, agent);
        if let Some(update) = terminal_update {
            runtime.sessions.apply_event(&session_name, update);
        }
    });

    Ok(())
}

/// 首轮结束后为会话自动生成标题；网页模型直接截取首条消息。
async fn auto_title_session(agent: &Agent, catalog: &Arc<dyn SessionCatalog>, name: &str) {
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
    // 网页模型的一次性补全会在 DeepSeek 侧新建独立对话。标题取首条消息，
    // 避免一次 Shirley 发言在网页侧产生第二个会话。
    let raw = if agent.model_config().model == "deepseek-web" {
        first.chars().take(20).collect::<String>()
    } else {
        let Ok(raw) = agent.complete(&title_prompt(&first)).await else {
            return;
        };
        raw
    };
    let Some(title) = sanitize_title(&raw) else {
        return;
    };

    let write_catalog = catalog.clone();
    let write_name = name.to_owned();
    let _ = tauri::async_runtime::spawn_blocking(move || write_catalog.rename(&write_name, &title))
        .await;
}

/// 订阅一个会话：返回其视图快照，并起一个 per-会话 pump 把后续事件转线格式 emit。
#[tauri::command]
async fn agent_subscribe(
    state: State<'_, DesktopState>,
    app: AppHandle,
    project: String,
    session: String,
) -> Result<SessionSnapshotWire, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let key = route_key(&project, &session);

    if let Some(handle) = state.pumps.lock().await.remove(&key) {
        handle.abort();
    }
    // **同一把锁内**先订阅再取快照：`apply_event` 也要拿这把锁，故两者之间不会
    // 插入任何事件——快照覆盖订阅前、通道覆盖订阅后，既不丢也不重。
    let (mut rx, items, running, todo) = {
        let guard = runtime.lock().await;
        let rx = guard
            .sessions
            .subscribe(&session)
            .ok_or_else(|| format!("会话不存在：{session}"))?;
        let items = guard
            .sessions
            .snapshot(&session)
            .ok_or_else(|| format!("会话不存在：{session}"))?;
        let running = guard.sessions.is_running(&session);
        // 同一把锁内连同任务账本快照一起取——切到会话时任务卡立即可见，不必等下一次
        // `todo` 工具完成。
        let todo = guard
            .sessions
            .session(&session)
            .and_then(|session| session.todo_view());
        (rx, items, running, todo)
    };

    let pumps = state.pumps.clone();
    let pump_project = project.clone();
    let pump_session = session.clone();
    let pump_key = key.clone();
    let pump_runtime = runtime.clone();
    let handle = tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Ok(event)) => {
                    // `todo` 工具完成 = 任务账本变更。SDK 不发专门事件，故这里在转发
                    // 工具事件之外，额外读一次账本快照发一条 `TodoUpdated` 给前端——
                    // 让「任务在推进」对用户可见（账本不解析注入 XML，取结构化视图）。
                    let is_todo = matches!(
                        &event,
                        AgentEvent::ToolFinished { name, .. } if name == "todo"
                    );
                    if let Some(wire) = AgentEventWire::from_event(event)
                        .map(|wire| wire.with_route(&pump_project, &pump_session))
                    {
                        let _ = app.emit(EVENT_NAME, wire);
                    }
                    if is_todo {
                        let todo = {
                            let guard = pump_runtime.lock().await;
                            guard
                                .sessions
                                .session(&pump_session)
                                .and_then(|session| session.todo_view())
                        };
                        let wire = AgentEventWire::TodoUpdated {
                            project: Some(pump_project.clone()),
                            session: Some(pump_session.clone()),
                            todo: todo.map(TaskViewWire::from),
                        };
                        let _ = app.emit(EVENT_NAME, wire);
                    }
                }
                Ok(Err(message)) => {
                    let wire = AgentEventWire::Error {
                        project: Some(pump_project.clone()),
                        session: Some(pump_session.clone()),
                        message,
                    };
                    let _ = app.emit(EVENT_NAME, wire);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        pumps.lock().await.remove(&pump_key);
    });
    let snapshot = SessionSnapshotWire::new(session.clone(), items, running, todo);
    state.pumps.lock().await.insert(key, handle);

    Ok(snapshot)
}

/// 停掉一个会话的 pump（切走时调用）。不存在静默成功（幂等）。
#[tauri::command]
async fn agent_unsubscribe(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
) -> Result<(), String> {
    let key = route_key(&project, &session);
    if let Some(handle) = state.pumps.lock().await.remove(&key) {
        handle.abort();
    }
    Ok(())
}

/// 当前模型名（该项目前台会话）。
///
/// 走会话级查询（[`SessionManager::active_model_name`]），**运行期也可用**：前台
/// 会话正在跑一轮时其 `Agent` 被 `take` 走，直接读 `active_agent()` 会拿到空串，
/// 前端就会把模型选择器清空——这正是「切走再切回模型不恢复」的表现。
#[tauri::command]
async fn agent_model_name(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<String, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let guard = runtime.lock().await;
    Ok(guard
        .sessions
        .active_model_name()
        .map(str::to_owned)
        .unwrap_or_default())
}

/// 列出当前项目端点可选的模型。
#[tauri::command]
async fn agent_list_models(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<ModelEntryWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let catalog = runtime.lock().await.factory.model_catalog.clone();
    let entries = catalog.list().await;
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
///
/// 同时按模型声明 **自动选工具协议**（`docs/tool-calling-refactor.md` Step 3）：
/// 从模型目录按 `model` 值反查条目，经 [`crate::bootstrap::AgentFactory::model_config_for`]
/// 算一份完整 `ModelConfig`（模型值 + 工具协议），再一次性下发
/// （[`shirley_agent_sdk::Agent::set_model_config`]）。
/// 前端不感知协议——它只传模型值，选择全在 Rust 侧。
#[tauri::command]
async fn agent_set_model(
    state: State<'_, DesktopState>,
    project: String,
    model: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    // 反查目录：先 clone 出 factory / catalog，再锁 sessions（避免持锁跨 await）。
    let (factory, catalog) = {
        let guard = runtime.lock().await;
        (guard.factory.clone(), guard.factory.model_catalog.clone())
    };
    let entry = catalog
        .list()
        .await
        .into_iter()
        .find(|entry| entry.value == model);
    // 按模型条目算一份完整 `ModelConfig`（模型值 + 工具协议），**一次性下发**
    // （`docs/tool-calling-refactor.md` Step 3）。目录里查不到就只改模型名。
    let config = entry.as_ref().map(|entry| factory.model_config_for(entry));

    let mut guard = runtime.lock().await;
    let session = guard.sessions.active_mut();
    let agent = session
        .agent
        .as_mut()
        .ok_or_else(|| "agent 正在运行中".to_owned())?;
    match config {
        Some(config) => agent.set_model_config(config),
        None => agent.set_model(model),
    }
    // 同步会话的模型名镜像，运行期切走再切回时仍能显示新模型。
    session.remember_model();
    Ok(())
}

/// `@` 引用的工作区文件检索（该项目的工作区根）。
#[tauri::command]
async fn agent_search_files(
    state: State<'_, DesktopState>,
    project: String,
    query: String,
) -> Result<Vec<FileEntryWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let root = runtime.lock().await.factory.working_dir.clone();
    let entries = tauri::async_runtime::spawn_blocking(move || {
        crate::workspace_search::search_files(&root, &query, None)
    })
    .await
    .map_err(|e| e.to_string())?;
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

/// 列出某项目当前会话可见的技能（`/` 候选用）。
///
/// 技能目录跨会话共享（装配时扫一次），故取该项目任一已加载会话的 `SkillRuntime`；
/// 优先当前前台会话。项目运行时尚未加载时，先 `ensure_runtime` 造出（会扫一次目录）。
#[tauri::command]
async fn agent_list_skills(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<SkillEntryWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let guard = runtime.lock().await;
    let skills = guard
        .sessions
        .active()
        .skills
        .as_ref()
        .map(|skills| skills.entries())
        .unwrap_or_default();
    Ok(skills
        .into_iter()
        .map(|(name, description)| SkillEntryWire { name, description })
        .collect())
}

/// 激活某项目指定会话的一个技能（`/<skill-name>` 入口，与 TUI 同语义）。
///
/// 只写该会话的 `SkillRuntime`：下一轮由 `SkillContextProvider` 追加注入其正文
/// （不新增注入接缝，`docs/skills.md` 5.6）。未知技能 / 读文件失败返回错误文案。
#[tauri::command]
async fn agent_activate_skill(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
    name: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let guard = runtime.lock().await;
    let session_ref = guard
        .sessions
        .session(&session)
        .ok_or_else(|| format!("会话不存在：{session}"))?;
    let skills = session_ref
        .skills
        .as_ref()
        .ok_or_else(|| "当前会话不支持技能".to_owned())?;
    skills
        .activate(&name)
        .map(|_| ())
        .map_err(|error| match error {
            crate::skills::runtime::ActivateError::UnknownSkill(name) => {
                format!("未知技能：{name}")
            }
            crate::skills::runtime::ActivateError::ReadFailed(reason) => reason,
        })
}

/// 列出某项目的全部会话（按最近修改降序）。
#[tauri::command]
async fn agent_list_sessions(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<SessionEntryWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let catalog = runtime.lock().await.factory.session_catalog.clone();
    let entries = tauri::async_runtime::spawn_blocking(move || catalog.list())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let guard = runtime.lock().await;
    Ok(entries
        .into_iter()
        .map(|entry| {
            let running = guard.sessions.is_running(&entry.name);
            let mut wire = SessionEntryWire::from(entry);
            wire.running = running;
            wire
        })
        .collect())
}

/// 某项目当前前台会话名（供前端展示 / 打标）。
#[tauri::command]
async fn agent_current_session(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Option<String>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    Ok(runtime
        .lock()
        .await
        .sessions
        .active_name()
        .map(str::to_owned))
}

/// 新建会话并可命名（`title` 为空 = 匿名）。经 `SessionManager` 惰性新建并切过去。
#[tauri::command]
async fn agent_new_session(
    state: State<'_, DesktopState>,
    project: String,
    title: Option<String>,
) -> Result<SessionEntryWire, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let name = runtime.lock().await.sessions.create_new(title.as_deref())?;
    Ok(SessionEntryWire {
        name: name.clone(),
        label: name,
        preview: String::new(),
        modified_ms: 0,
        turns: 0,
        running: false,
    })
}

/// 切换到指定会话（经 `SessionManager` 挪 `active` 指针）。
#[tauri::command]
async fn agent_switch_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    runtime.lock().await.sessions.switch_to(&name)?;
    Ok(())
}

/// 重命名会话：**只改标题，不改标识 / 内容**。
#[tauri::command]
async fn agent_rename_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
    title: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let catalog = runtime.lock().await.factory.session_catalog.clone();
    tauri::async_runtime::spawn_blocking(move || catalog.rename(&name, &title))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// 删除会话（连同日志与标题）。运行中拒绝。
#[tauri::command]
async fn agent_delete_session(
    state: State<'_, DesktopState>,
    project: String,
    name: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    runtime.lock().await.sessions.delete_session(&name)?;
    let catalog = runtime.lock().await.factory.session_catalog.clone();
    let delete_name = name.clone();
    tauri::async_runtime::spawn_blocking(move || catalog.delete(&delete_name))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 回放某项目当前会话历史。
#[tauri::command]
async fn agent_load_history(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<Vec<HistoryMessageWire>, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let guard = runtime.lock().await;
    let agent = guard
        .sessions
        .active_agent()
        .ok_or_else(|| "agent 正在运行中".to_owned())?;
    Ok(history_from_messages(agent.messages()))
}

/// 取消指定会话正在跑的一轮。
///
/// 取出 `agent_send` 为该会话登记的取消句柄并发信号；驱动 `run_stream` 的任务
/// 收到后 drop 掉流、回退本轮 Agent 状态，并以 `Finished(Cancelled)` 收尾。
/// 会话不在运行（句柄已移除 / 从未发起）时静默成功（幂等）。
#[tauri::command]
async fn agent_cancel(
    state: State<'_, DesktopState>,
    project: String,
    session: String,
) -> Result<(), String> {
    let key = route_key(&project, &session);
    if let Some(cancel) = state.cancels.lock().await.remove(&key) {
        // `send` 失败 = 任务已结束（接收端已 drop），无害。
        let _ = cancel.send(());
    }
    Ok(())
}

pub fn run(factory: AgentFactory) -> std::io::Result<()> {
    let factory = Arc::new(factory);
    let working_dir = factory.working_dir.clone();

    // 把当前工作区登记为默认项目（幂等：已登记则只刷新打开时间）。
    let mut registry =
        ProjectRegistry::load_default().map_err(|e| std::io::Error::other(e.to_string()))?;
    let default_entry = registry
        .add(working_dir, None)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    eprintln!(
        "[desktop] 默认项目：{}（{}）",
        default_entry.name, default_entry.id
    );

    let runtime = runtime_from_factory(factory).map_err(std::io::Error::other)?;
    let mut runtimes = HashMap::new();
    runtimes.insert(default_entry.id.clone(), Arc::new(Mutex::new(runtime)));

    let state = DesktopState {
        registry: Arc::new(Mutex::new(registry)),
        runtimes: Arc::new(Mutex::new(runtimes)),
        pumps: Arc::new(Mutex::new(HashMap::new())),
        cancels: Arc::new(Mutex::new(HashMap::new())),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            agent_list_projects,
            agent_add_project,
            agent_rename_project,
            agent_archive_project,
            agent_restore_project,
            agent_send,
            agent_subscribe,
            agent_unsubscribe,
            agent_cancel,
            agent_model_name,
            agent_list_models,
            agent_set_model,
            agent_search_files,
            agent_list_skills,
            agent_activate_skill,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::FileSessionCatalog;

    fn tmp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shirley_shell_{tag}_{}", std::process::id()))
    }

    /// 回归：懒加载一个**已有会话**的项目时，必须恢复其最近修改的会话，
    /// 而不是凭空造一个空会话——否则前端切项目会把选中 session 变成 new session。
    #[test]
    fn runtime_from_factory_resumes_latest_session() {
        let root = tmp_root("resume_latest");
        let _ = std::fs::remove_dir_all(&root);

        let catalog = FileSessionCatalog::new(&root);
        let (older, older_store) = catalog.create_named(Some("旧会话")).unwrap();
        older_store
            .append(&Message::User {
                content: "旧".into(),
            })
            .unwrap();
        // 拉开 mtime，保证「最近修改」判定不因同毫秒而抖动。
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (newer, newer_store) = catalog.create_named(Some("新会话")).unwrap();
        newer_store
            .append(&Message::User {
                content: "新".into(),
            })
            .unwrap();

        let factory = Arc::new(AgentFactory::assemble(root.clone()).unwrap());
        let runtime = runtime_from_factory(factory).unwrap();

        assert_eq!(
            runtime.sessions.active_name(),
            Some(newer.name.as_str()),
            "应恢复最近修改的会话，而不是新建空会话"
        );
        assert_ne!(runtime.sessions.active_name(), Some(older.name.as_str()));
        // 恢复的会话视图条目应已重建（含历史），否则切回来会看到空白。
        let snapshot = runtime.sessions.snapshot(&newer.name).unwrap();
        assert!(!snapshot.is_empty(), "恢复会话应带历史条目");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 项目**没有任何会话**时才惰性开一份空会话（只定名、不落盘）。
    #[test]
    fn runtime_from_factory_creates_empty_session_when_none_exists() {
        let root = tmp_root("resume_empty");
        let _ = std::fs::remove_dir_all(&root);

        let factory = Arc::new(AgentFactory::assemble(root.clone()).unwrap());
        let runtime = runtime_from_factory(factory).unwrap();

        assert!(runtime.sessions.active_name().is_some(), "应开出一份空会话");
        // 惰性会话此刻未落盘：会话目录里不应有任何 jsonl。
        assert!(
            FileSessionCatalog::new(&root).list().unwrap().is_empty(),
            "惰性空会话不应立即落盘"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
