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
//! - command `agent_cancel`：预留（`StopReason::Cancelled` 尚未产生，见 `Agent.md` 缺口 9）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use serde::Serialize;
use shirley_agent_sdk::{Agent, Message};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;

use crate::interface::session::SessionManager;
use super::wire::{AgentEventWire, SessionSnapshotWire};
use crate::bootstrap::AgentFactory;
use crate::models::ModelCatalog;
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
/// - `model_catalog`：模型目录（全局，取默认项目的工厂）。
/// - `pumps`：每个「项目::会话」的事件 pump 句柄。
struct DesktopState {
    registry: Arc<Mutex<ProjectRegistry>>,
    runtimes: Arc<Mutex<HashMap<String, Arc<Mutex<ProjectRuntime>>>>>,
    model_catalog: Arc<dyn ModelCatalog>,
    pumps: Arc<Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>>,
}

/// 事件 pump 的键：跨项目不重名（会话名在项目内唯一，项目 id 全局唯一）。
fn route_key(project: &str, session: &str) -> String {
    format!("{project}::{session}")
}

/// 用一份现成工厂造一个项目运行时：惰性开一份空会话（此刻只定名、不落盘）。
fn runtime_from_factory(factory: Arc<AgentFactory>) -> Result<ProjectRuntime, String> {
    let catalog = factory.session_catalog.clone();
    let (entry, store) = catalog.create_lazy(None).map_err(|e| e.to_string())?;
    let log = store.load().map_err(|e| e.to_string())?;
    let built = factory.build_agent(log).map_err(|e| e.to_string())?;
    let sessions = SessionManager::with_factory(
        built.agent,
        Some(entry.name),
        catalog,
        factory.clone(),
        Some(store),
        Some(built.memory),
        Some(built.skills),
    );
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
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog().file().blocking_pick_folder()
    })
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
    tauri::async_runtime::spawn(async move {
        if let Some(memory) = memory.as_ref() {
            let _ = memory.prefetch_query_vector().await;
        }
        {
            let mut stream = agent.run_stream(&prompt);
            while let Some(event) = stream.next().await {
                let update = match event {
                    Ok(event) => Ok(event),
                    Err(error) => Err(error.to_string()),
                };
                runtime_for_task
                    .lock()
                    .await
                    .sessions
                    .apply_event(&session_name, update);
            }
        }
        auto_title_session(&agent, &catalog, &session_name).await;
        runtime_for_task
            .lock()
            .await
            .sessions
            .restore_agent(&session_name, agent);
    });

    Ok(())
}

/// 首轮结束后为会话自动生成标题（AI 命名）。
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
    let (mut rx, items, running) = {
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
        (rx, items, running)
    };

    let pumps = state.pumps.clone();
    let pump_project = project.clone();
    let pump_session = session.clone();
    let pump_key = key.clone();
    let handle = tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Ok(event)) => {
                    let wire = AgentEventWire::from_event(event)
                        .with_route(&pump_project, &pump_session);
                    let _ = app.emit(EVENT_NAME, wire);
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
    let snapshot = SessionSnapshotWire::new(session.clone(), items, running);
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

/// 当前模型名（取该项目前台会话的 Agent）。
#[tauri::command]
async fn agent_model_name(
    state: State<'_, DesktopState>,
    project: String,
) -> Result<String, String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let guard = runtime.lock().await;
    Ok(guard
        .sessions
        .active_agent()
        .map(|agent| agent.model_config().model.clone())
        .unwrap_or_default())
}

/// 列出可选模型（模型目录全局，取自默认项目的工厂）。
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
async fn agent_set_model(
    state: State<'_, DesktopState>,
    project: String,
    model: String,
) -> Result<(), String> {
    let runtime = ensure_runtime(&state, &project).await?;
    let mut guard = runtime.lock().await;
    let agent = guard
        .sessions
        .active_agent_mut()
        .ok_or_else(|| "agent 正在运行中".to_owned())?;
    agent.set_model(model);
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
    let name = runtime
        .lock()
        .await
        .sessions
        .create_new(title.as_deref())?;
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

/// 预留：取消本轮运行（SDK 侧取消机制尚未落地，见 `Agent.md` 缺口 9）。
#[tauri::command]
fn agent_cancel() {}

pub fn run(factory: AgentFactory) -> std::io::Result<()> {
    let factory = Arc::new(factory);
    let working_dir = factory.working_dir.clone();
    let model_catalog = factory.model_catalog.clone();

    // 把当前工作区登记为默认项目（幂等：已登记则只刷新打开时间）。
    let mut registry = ProjectRegistry::load_default()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let default_entry = registry
        .add(working_dir, None)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    eprintln!("[desktop] 默认项目：{}（{}）", default_entry.name, default_entry.id);

    let runtime = runtime_from_factory(factory)
        .map_err(std::io::Error::other)?;
    let mut runtimes = HashMap::new();
    runtimes.insert(default_entry.id.clone(), Arc::new(Mutex::new(runtime)));

    let state = DesktopState {
        registry: Arc::new(Mutex::new(registry)),
        runtimes: Arc::new(Mutex::new(runtimes)),
        model_catalog,
        pumps: Arc::new(Mutex::new(HashMap::new())),
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
