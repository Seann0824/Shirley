//! Tauri 2 壳：把 `Bootstrap` 的 `Agent` 接进 webview。
//!
//! 事件契约（与 `web/src/lib/bridge.ts` 对齐）：
//! - command `agent_send { text, references }`：发起一轮 `run_stream`，事件经
//!   `agent://event` 推送。`references` 是 `@` 引用的工作区路径，会拼进本轮 prompt。
//! - command `agent_cancel`：预留（`StopReason::Cancelled` 尚未产生，见 `Agent.md` 缺口 9）。
//! - command `agent_model_name`：页脚 / 选择器展示当前模型名。
//! - command `agent_list_models`：列出可选模型（`ModelCatalog`）。
//! - command `agent_set_model`：热切换模型（`Agent::set_model`，不重建 Agent）。
//! - command `agent_search_files { query }`：`@` 引用的工作区文件检索（应用层，不经 SDK）。

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use serde::Serialize;
use shirley_agent_sdk::{Agent, Message};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

use super::wire::AgentEventWire;
use crate::bootstrap::Bootstrap;
use crate::models::ModelCatalog;
use crate::session::{SessionCatalog, SessionEntry};

/// webview 侧监听的统一事件名。
const EVENT_NAME: &str = "agent://event";

/// Tauri 管理的运行时状态。
///
/// `Agent` 放在 `Option` 里：一轮运行期间把它 `take` 出去移入后台任务，
/// 结束后放回——与 TUI 的 `take_agent` / `restore_agent` 同款手法，
/// 避免同一 `Agent` 被并发驱动。
struct DesktopState {
    agent: Arc<Mutex<Option<Agent>>>,
    model_catalog: Arc<dyn ModelCatalog>,
    /// 会话目录（`/session` 切换 / 重命名 / 删除用）——与 TUI **共用同一个实现**，
    /// 因此两边读的是同一份 `<root>/.shirley/sessions` 数据。
    session_catalog: Arc<dyn SessionCatalog>,
    /// 当前会话名（切换 / 新建后更新；供前端展示与"当前会话"打标）。
    current_session: Arc<Mutex<Option<String>>>,
    /// 工作区根目录：`@` 文件检索用（应用层 `workspace_search`，不碰 SDK）。
    working_dir: PathBuf,
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
}

impl From<SessionEntry> for SessionEntryWire {
    fn from(entry: SessionEntry) -> Self {
        Self {
            name: entry.name,
            label: entry.label,
            preview: entry.preview,
            modified_ms: entry.modified_ms,
            turns: entry.turns,
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
    app: AppHandle,
    text: String,
    references: Option<Vec<String>>,
) -> Result<(), String> {
    let slot = state.agent.clone();
    let mut agent = slot
        .lock()
        .await
        .take()
        .ok_or_else(|| "agent 正在运行中".to_owned())?;

    let prompt = compose_prompt(&text, references.as_deref().unwrap_or(&[]));

    tauri::async_runtime::spawn(async move {
        {
            let mut stream = agent.run_stream(&prompt);
            while let Some(event) = stream.next().await {
                let wire = match event {
                    Ok(event) => AgentEventWire::from_event(event),
                    Err(error) => AgentEventWire::Error {
                        message: error.to_string(),
                    },
                };
                let _ = app.emit(EVENT_NAME, wire);
            }
        }
        // 无论成败都把 Agent 放回，供下一轮复用。
        *slot.lock().await = Some(agent);
    });

    Ok(())
}

#[tauri::command]
async fn agent_model_name(state: State<'_, DesktopState>) -> Result<String, String> {
    Ok(state
        .agent
        .lock()
        .await
        .as_ref()
        .map(|agent| agent.model_config().model.clone())
        .unwrap_or_default())
}

/// 列出可选模型（模型目录由 `Bootstrap` 注入）。
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
    let mut guard = state.agent.lock().await;
    let agent = guard.as_mut().ok_or_else(|| "agent 正在运行中".to_owned())?;
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
    Ok(entries.into_iter().map(SessionEntryWire::from).collect())
}

/// 当前会话名（供前端展示 / 打标）。
#[tauri::command]
async fn agent_current_session(state: State<'_, DesktopState>) -> Result<Option<String>, String> {
    Ok(state.current_session.lock().await.clone())
}

/// 新建会话并可命名（`title` 为空 = 匿名，回落到首条用户消息 / 名字）。
///
/// 新建后立即 `switch_session` 切过去——与 TUI 选择器「＋ 新建会话」同语义。
#[tauri::command]
async fn agent_new_session(
    state: State<'_, DesktopState>,
    title: Option<String>,
) -> Result<SessionEntryWire, String> {
    let catalog = state.session_catalog.clone();
    let (entry, store) = tauri::async_runtime::spawn_blocking(move || {
        catalog.create_named(title.as_deref())
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;

    {
        let mut guard = state.agent.lock().await;
        let agent = guard.as_mut().ok_or_else(|| "agent 正在运行中".to_owned())?;
        agent.switch_session(store).map_err(|error| error.to_string())?;
    }
    *state.current_session.lock().await = Some(entry.name.clone());
    Ok(entry.into())
}

/// 切换到指定会话（`Agent::switch_session`，与 TUI 同一接缝）。
#[tauri::command]
async fn agent_switch_session(
    state: State<'_, DesktopState>,
    name: String,
) -> Result<(), String> {
    let catalog = state.session_catalog.clone();
    let open_name = name.clone();
    let store = tauri::async_runtime::spawn_blocking(move || catalog.open(&open_name))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;

    {
        let mut guard = state.agent.lock().await;
        let agent = guard.as_mut().ok_or_else(|| "agent 正在运行中".to_owned())?;
        agent.switch_session(store).map_err(|error| error.to_string())?;
    }
    *state.current_session.lock().await = Some(name);
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
#[tauri::command]
async fn agent_delete_session(
    state: State<'_, DesktopState>,
    name: String,
) -> Result<(), String> {
    {
        let guard = state.agent.lock().await;
        if guard.is_none() {
            return Err("agent 正在运行中".to_owned());
        }
    }
    let catalog = state.session_catalog.clone();
    let delete_name = name.clone();
    tauri::async_runtime::spawn_blocking(move || catalog.delete(&delete_name))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    // 删掉的若是当前会话，清空当前标记（前端据此重置视图）。
    let mut current = state.current_session.lock().await;
    if current.as_deref() == Some(name.as_str()) {
        *current = None;
    }
    Ok(())
}

/// 回放当前会话历史（切换 / 启动后重建 transcript 用）。
#[tauri::command]
async fn agent_load_history(
    state: State<'_, DesktopState>,
) -> Result<Vec<HistoryMessageWire>, String> {
    let guard = state.agent.lock().await;
    let agent = guard.as_ref().ok_or_else(|| "agent 正在运行中".to_owned())?;
    Ok(history_from_messages(agent.messages()))
}

/// 预留：取消本轮运行（SDK 侧取消机制尚未落地，见 `Agent.md` 缺口 9）。
#[tauri::command]
fn agent_cancel() {}

pub fn run(bootstrap: Bootstrap) -> std::io::Result<()> {
    let Bootstrap {
        agent,
        model_catalog,
        session_catalog,
        current_session,
        working_dir,
        ..
    } = bootstrap;
    if let Some(name) = &current_session {
        eprintln!("[desktop] 会话：{name}");
    }

    let state = DesktopState {
        agent: Arc::new(Mutex::new(Some(agent))),
        model_catalog,
        session_catalog,
        current_session: Arc::new(Mutex::new(current_session)),
        working_dir,
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            agent_send,
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
