//! Subagent runtime manager (application layer, `docs/subagent.md` decisions 6/7/8).
//!
//! One manager holds the runtime state of every subagent dispatched by **one session**, plus
//! the ability to create child `Agent`s (an `Arc<AgentFactory>`). Subagents are same-process
//! `tokio::spawn` tasks; the only reason for the separate task (rather than `run().await`
//! directly inside the tool future) is **cancellability** (decision 7) -- the parent can
//! `AbortHandle::abort()` an out-of-control subtask.
//!
//! Zero SDK change: the child is an ordinary `Agent` built by the factory, the dispatch entry is
//! an ordinary `Tool`, and the "sleep-wakeup" is just the tool future staying `Pending` while the
//! child runs (the `ReAct` runtime's `FuturesUnordered` awaits it naturally).
//!
//! P1 adds: `wait` / `detach` / `timeout_ms`, `cancel` wired to a tool action, persistence hooks
//! (the [`SubagentStore`] sidecar) and a per-subagent `broadcast` channel so the UI can observe a
//! running child (decision 10, path 2 -- the SDK's `Tool` cannot emit `AgentEvent`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use shirley_agent_sdk::{AgentError, AgentEvent, Message, ModelConfig, RunResult, Usage};
use tokio::sync::broadcast;

use crate::bootstrap::AgentFactory;

use super::store::{IndexRecord, SubagentStore};

/// Default caps (decision 8). P0 reads these directly; the `[subagent]` config table is P1.
pub const DEFAULT_MAX_CONCURRENT: usize = 4;
/// `max_depth = 1` means "a child cannot dispatch a child" (the P0 default). Enforced structurally
/// (the child tool table never contains `subagent`) plus this numeric guard as a belt-and-suspenders.
pub const DEFAULT_MAX_DEPTH: usize = 1;
/// A single child's execution-step budget, independent of the parent.
pub const DEFAULT_PER_TASK_MAX_STEPS: usize = 24;
/// Default `wait` timeout (10 minutes) when the model omits `timeout_ms`.
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 600_000;
/// Upper bound on a caller-supplied `timeout_ms` (1 hour).
pub const MAX_WAIT_TIMEOUT_MS: u64 = 3_600_000;

/// Task length cap (chars), same convention as `read`'s argument validation.
const MAX_TASK_CHARS: usize = 4000;
/// Display-name cap (chars); longer names are truncated with an ellipsis.
const MAX_NAME_CHARS: usize = 60;
/// Per-subagent event channel capacity (same order as `Session::events`).
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// The three caps (decision 8). P0 uses [`SubagentLimits::default`].
#[derive(Clone, Debug)]
pub struct SubagentLimits {
    /// Max subagents running at the same time; beyond this a dispatch is rejected outright
    /// (never silently queued).
    pub max_concurrent: usize,
    /// Max nesting depth. `1` = a child cannot dispatch a child.
    pub max_depth: usize,
    /// A single child's model-call budget.
    pub per_task_max_steps: usize,
}

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            max_depth: DEFAULT_MAX_DEPTH,
            per_task_max_steps: DEFAULT_PER_TASK_MAX_STEPS,
        }
    }
}

/// The child tool table selection (decision 5): `(allow ?? default) - deny - {subagent}`.
#[derive(Clone, Debug, Default)]
pub struct ToolSelection {
    /// Allowlist of tool names (`None` = the configured default set = all tools).
    pub allow: Option<Vec<String>>,
    /// Denylist, subtracted from the default set.
    pub deny: Vec<String>,
}

/// How a subagent was created: `spawn` (clean context) or `fork` (derived from the parent snapshot).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubagentKind {
    Spawn,
    Fork,
}

impl SubagentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Fork => "fork",
        }
    }

    /// Parse the `kind` field recorded in `index.jsonl` (unknown -> `None`).
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "spawn" => Some(Self::Spawn),
            "fork" => Some(Self::Fork),
            _ => None,
        }
    }
}

/// State machine: `Running -> Completed | Failed | Cancelled`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubagentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl SubagentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse the `status` field recorded in `index.jsonl` (unknown -> `None`).
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A finished subagent's conclusion + cost.
#[derive(Clone, Debug)]
pub struct SubagentResult {
    /// The child's final assistant body (the last non-empty assistant text).
    pub summary: String,
    /// `Debug` rendering of the child's `StopReason` (e.g. `Completed` / `MaxStepsReached`).
    pub stop_reason: String,
    /// The child's own usage. **Kept separate** from the parent session's `total_usage` (open
    /// question 5 leans "separate statistics"); the parent's context-usage reading is not misled
    /// by the child's consumption.
    pub usage: Usage,
}

/// A cloneable snapshot of one subagent (the live `JoinHandle` / `AbortHandle` are not exposed).
#[derive(Clone, Debug)]
pub struct SubagentInfo {
    pub id: String,
    pub kind: SubagentKind,
    /// A short human-readable label ("what is this agent doing"). Model-supplied when given,
    /// otherwise derived from the first line of `task`. Purely for display.
    pub name: String,
    pub task: String,
    pub status: SubagentStatus,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub result: Option<SubagentResult>,
}

impl SubagentInfo {
    pub fn elapsed_ms(&self) -> u64 {
        self.finished_ms
            .unwrap_or_else(now_ms)
            .saturating_sub(self.started_ms)
    }
}

/// The outcome of [`SubagentManager::wait`].
#[derive(Clone, Debug)]
pub struct WaitOutcome {
    pub info: SubagentInfo,
    /// `true` when the timeout elapsed while the child was still running (the child keeps going).
    pub timed_out: bool,
}

/// A real-time event for one subagent (`docs/subagent.md` decision 10, path 2).
///
/// The application layer owns this channel (the SDK's `Tool` cannot emit `AgentEvent`), so the UI
/// subscribes directly to the manager rather than the SDK. Whether or not anyone subscribes, the
/// child still runs -- `broadcast::send` with no receiver is simply dropped.
#[derive(Clone, Debug)]
pub enum SubagentEvent {
    /// A stream event forwarded from the child's own `run_stream` (deltas, tool calls, ...).
    Agent(AgentEvent),
    /// The child reached a terminal state (`completed` / `failed` / `cancelled`).
    Status(SubagentStatus),
}

struct SubagentHandle {
    info: SubagentInfo,
    /// The spawned task's handle. `take`n out by the blocking tool future to await completion;
    /// dropping it detaches (does not abort) the task, which is what non-blocking actions want.
    join: Option<tokio::task::JoinHandle<()>>,
    /// For cancel (decision 7).
    abort: Option<tokio::task::AbortHandle>,
}

/// Dispatch-time error, mapped by [`crate::subagent::SubagentTool`] to `ToolError`.
#[derive(Debug)]
pub enum SubagentError {
    /// A tool name that is not in the available set (or is `subagent` itself -- recursion guard).
    UnknownTool(String),
    TaskRequired,
    TaskTooLong,
    DepthExceeded,
    ConcurrencyExceeded,
    /// The requested id is not in the registry (`wait` / `status` / `cancel`).
    NotFound(String),
    Build(String),
}

impl std::fmt::Display for SubagentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(
                f,
                "unknown tool `{name}` (allowed: the available tool names; `subagent` cannot be \
                 delegated to a child)"
            ),
            Self::TaskRequired => write!(f, "`task` is required for spawn/fork"),
            Self::TaskTooLong => write!(f, "`task` exceeds the {MAX_TASK_CHARS}-char cap"),
            Self::DepthExceeded => write!(
                f,
                "max_depth reached: a subagent cannot dispatch a subagent"
            ),
            Self::ConcurrencyExceeded => {
                write!(
                    f,
                    "too many subagents running at once (max_concurrent); wait for one to finish"
                )
            }
            Self::NotFound(id) => write!(f, "no such subagent: {id}"),
            Self::Build(detail) => write!(f, "failed to create the subagent: {detail}"),
        }
    }
}

/// One manager per session (`docs/subagent.md` decision 6): the single source of truth for that
/// session's subagents.
pub struct SubagentManager {
    registry: Mutex<HashMap<String, SubagentHandle>>,
    factory: Arc<AgentFactory>,
    limits: SubagentLimits,
    next_seq: AtomicU64,
    /// A **derived mirror** of the parent session's working set, used only as the `fork` snapshot
    /// source (the SDK exposes no mid-run message handle to a `Tool`). Seeded from
    /// `Agent::messages()` and kept current by `Session::apply_event` on every `MessageAdded`.
    parent_view: Mutex<Vec<Message>>,
    parent_model: Mutex<Option<ModelConfig>>,
    /// The names the parent may hand to a child (used to reject unknown tool names outright,
    /// decision 5). `subagent` is never in this list.
    available_tools: Vec<String>,
    /// The default child tool set used when the model does not pass an explicit `tools` allowlist
    /// (`[subagent].default_tools`). `None` = all available tools.
    default_tools: Option<Vec<String>>,
    /// Default `wait` / blocking-dispatch timeout (`[subagent].default_timeout_ms`).
    default_timeout_ms: u64,
    /// The session's `.agents/` sidecar (decision 9). `None` = persistence disabled (tests / a
    /// session that has not materialized a store yet). Settable after construction because the
    /// store needs the session name, which only the session container knows.
    store: Mutex<Option<SubagentStore>>,
    /// Per-subagent real-time channels (decision 10, path 2). Created at dispatch; a subscriber
    /// that arrives late simply sees no history (the UI can `load_transcript` for that).
    events: Mutex<HashMap<String, broadcast::Sender<SubagentEvent>>>,
}

impl SubagentManager {
    pub fn set_parent_model(&self, config: ModelConfig) {
        *self.parent_model.lock().unwrap() = Some(config);
    }
    pub fn new(
        factory: Arc<AgentFactory>,
        limits: SubagentLimits,
        available_tools: Vec<String>,
    ) -> Self {
        Self::with_store(factory, limits, available_tools, None)
    }

    /// Build with a persistence sidecar. Any subagent already recorded in `index.jsonl` is seeded
    /// into the registry (so `list` / `status` still work after a session restore); a record that
    /// was left `running` is reported as `failed` ("process exited") -- no checkpoint-resume.
    pub fn with_store(
        factory: Arc<AgentFactory>,
        limits: SubagentLimits,
        available_tools: Vec<String>,
        store: Option<SubagentStore>,
    ) -> Self {
        let manager = Self {
            registry: Mutex::new(HashMap::new()),
            factory,
            limits,
            next_seq: AtomicU64::new(0),
            parent_view: Mutex::new(Vec::new()),
            parent_model: Mutex::new(None),
            available_tools,
            default_tools: None,
            default_timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
            store: Mutex::new(store),
            events: Mutex::new(HashMap::new()),
        };
        manager.restore_from_index();
        manager
    }

    pub fn limits(&self) -> &SubagentLimits {
        &self.limits
    }

    /// The tool names a child may be granted (`subagent` is never included).
    pub fn available_tools(&self) -> &[String] {
        &self.available_tools
    }

    /// Set the default child tool set (`[subagent].default_tools`). Call before wrapping in `Arc`.
    pub fn set_default_tools(&mut self, tools: Option<Vec<String>>) {
        self.default_tools = tools;
    }

    /// Set the default `wait` timeout (`[subagent].default_timeout_ms`). Call before wrapping in `Arc`.
    pub fn set_default_timeout_ms(&mut self, ms: u64) {
        self.default_timeout_ms = ms;
    }

    /// The default `wait` timeout (ms).
    pub fn default_timeout_ms(&self) -> u64 {
        self.default_timeout_ms
    }

    /// The persistence sidecar, when configured (cloned; the store is a cheap handle).
    pub fn store(&self) -> Option<SubagentStore> {
        self.store.lock().unwrap().clone()
    }

    /// Attach (or replace) the persistence sidecar once the session name is known, then seed the
    /// registry from any existing `index.jsonl` (so `list` / `status` survive a session restore).
    pub fn set_store(&self, store: SubagentStore) {
        *self.store.lock().unwrap() = Some(store);
        self.restore_from_index();
    }

    /// Seed the fork snapshot from the parent's restored working set (called once at session build).
    pub fn set_parent_view(&self, messages: Vec<Message>) {
        *self.parent_view.lock().unwrap() = messages;
    }

    /// Append one message to the fork snapshot (called from `Session::apply_event` on `MessageAdded`).
    pub fn push_parent_message(&self, message: &Message) {
        self.parent_view.lock().unwrap().push(message.clone());
    }

    /// Dispatch one subagent. Returns its id. The child runs in a `tokio::spawn` task.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "task is cloned into the child handle and moved into the spawned task"
    )]
    pub fn dispatch(
        self: &Arc<Self>,
        kind: SubagentKind,
        name: Option<String>,
        task: String,
        tools: Option<Vec<String>>,
        deny_tools: Option<Vec<String>>,
        depth: usize,
    ) -> Result<String, SubagentError> {
        if task.trim().is_empty() {
            return Err(SubagentError::TaskRequired);
        }
        if task.chars().count() > MAX_TASK_CHARS {
            return Err(SubagentError::TaskTooLong);
        }
        if depth >= self.limits.max_depth {
            return Err(SubagentError::DepthExceeded);
        }

        // Validate the requested tool names against the available set. An unknown name is rejected
        // outright (the model must be able to find out immediately); `subagent` itself can never be
        // delegated (recursion guard).
        let deny = deny_tools.unwrap_or_default();
        // An omitted allowlist falls back to the configured default set (all tools when unset).
        let tools = tools.or_else(|| self.default_tools.clone());
        for name in tools.iter().flatten().chain(deny.iter()) {
            if name == "subagent" || !self.available_tools.contains(name) {
                return Err(SubagentError::UnknownTool(name.clone()));
            }
        }

        // Concurrency guard: reject outright rather than queue (decision 8).
        {
            let registry = self.registry.lock().unwrap();
            let running = registry
                .values()
                .filter(|handle| handle.info.status == SubagentStatus::Running)
                .count();
            if running >= self.limits.max_concurrent {
                return Err(SubagentError::ConcurrencyExceeded);
            }
        }

        let selection = ToolSelection { allow: tools, deny };
        let messages = match kind {
            SubagentKind::Spawn => Vec::new(),
            SubagentKind::Fork => fork_snapshot(&self.parent_view.lock().unwrap()),
        };
        let mut built = self
            .factory
            .build_agent_with(
                messages,
                Some(&selection),
                Some(self.limits.per_task_max_steps),
            )
            .map_err(|error| SubagentError::Build(error.to_string()))?;
        if let Some(config) = self.parent_model.lock().unwrap().clone() {
            built.agent.set_model_config(config);
        }

        let id = format!("sa_{:x}", self.next_seq.fetch_add(1, Ordering::Relaxed) + 1);
        let started_ms = now_ms();
        let name = resolve_name(name, &task);
        let info = SubagentInfo {
            id: id.clone(),
            kind,
            name: name.clone(),
            task: task.clone(),
            status: SubagentStatus::Running,
            started_ms,
            finished_ms: None,
            result: None,
        };
        self.registry.lock().unwrap().insert(
            id.clone(),
            SubagentHandle {
                info,
                join: None,
                abort: None,
            },
        );
        // The real-time channel exists for the whole lifetime of the subagent (so a subscriber that
        // arrives while the child is running does not miss the terminal `Status`).
        let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        self.events.lock().unwrap().insert(id.clone(), sender);
        self.record_started(&id, kind, &name, &task, started_ms);

        let manager = Arc::clone(self);
        let child_id = id.clone();
        let child_task = task.clone();
        let join = tokio::spawn(async move {
            // Stream the child so its events can bubble to a UI subscriber (decision 10). With no
            // subscriber, `send` is dropped -- the child still runs to completion.
            let mut outcome: Option<Result<RunResult, AgentError>> = None;
            {
                let mut stream = built.agent.run_stream(&child_task);
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(AgentEvent::Finished(result)) => {
                            outcome = Some(Ok(result.clone()));
                            manager.emit(
                                &child_id,
                                SubagentEvent::Agent(AgentEvent::Finished(result)),
                            );
                        }
                        Ok(event) => manager.emit(&child_id, SubagentEvent::Agent(event)),
                        Err(error) => {
                            outcome = Some(Err(error));
                            break;
                        }
                    }
                }
            }
            // Persist the child's own transcript (best-effort; a failure only costs the preview).
            if let Some(store) = manager.store() {
                let transcript = built.agent.messages().to_vec();
                let _ = store.write_transcript(&child_id, &transcript);
            }
            let outcome = outcome.unwrap_or_else(|| {
                Err(AgentError::Other(
                    "subagent stream ended without a final result".into(),
                ))
            });
            manager.finish(&child_id, outcome);
        });
        let abort = join.abort_handle();
        {
            let mut registry = self.registry.lock().unwrap();
            if let Some(handle) = registry.get_mut(&id) {
                handle.join = Some(join);
                handle.abort = Some(abort);
            }
        }
        Ok(id)
    }

    /// Block until the subagent's task ends (the parent "sleeps" here). Safe to call once per id:
    /// the join handle is `take`n out, so a second call returns immediately.
    pub async fn await_completion(&self, id: &str) {
        let join = {
            let mut registry = self.registry.lock().unwrap();
            registry.get_mut(id).and_then(|handle| handle.join.take())
        };
        if let Some(join) = join {
            let _ = join.await;
        }
    }

    /// Wait for a subagent to reach a terminal state, up to `timeout`.
    ///
    /// Uses the per-subagent broadcast channel (not the join handle), so it is **safe to call more
    /// than once** and a timeout does not detach the task from future waits. Returns immediately if
    /// the child already finished; on timeout the child keeps running and `timed_out` is `true`.
    pub async fn wait(
        &self,
        id: &str,
        timeout: Option<Duration>,
    ) -> Result<WaitOutcome, SubagentError> {
        // Subscribe **before** checking status, so a terminal transition between the two is not
        // missed (the status check catches it if it already happened; otherwise the event arrives).
        let mut receiver = {
            let registry = self.registry.lock().unwrap();
            let handle = registry
                .get(id)
                .ok_or_else(|| SubagentError::NotFound(id.to_owned()))?;
            if handle.info.status != SubagentStatus::Running {
                return Ok(WaitOutcome {
                    info: handle.info.clone(),
                    timed_out: false,
                });
            }
            self.events
                .lock()
                .unwrap()
                .get(id)
                .map(tokio::sync::broadcast::Sender::subscribe)
                .ok_or_else(|| SubagentError::NotFound(id.to_owned()))?
        };
        let wait = async {
            loop {
                match receiver.recv().await {
                    Ok(SubagentEvent::Status(_)) | Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        };
        let timed_out = if let Some(duration) = timeout {
            tokio::time::timeout(duration, wait).await.is_err()
        } else {
            wait.await;
            false
        };
        let info = self
            .info(id)
            .ok_or_else(|| SubagentError::NotFound(id.to_owned()))?;
        Ok(WaitOutcome { info, timed_out })
    }

    /// Subscribe to a subagent's real-time event stream (decision 10, path 2).
    pub fn subscribe(&self, id: &str) -> Option<broadcast::Receiver<SubagentEvent>> {
        self.events
            .lock()
            .unwrap()
            .get(id)
            .map(tokio::sync::broadcast::Sender::subscribe)
    }

    /// A snapshot of one subagent.
    pub fn info(&self, id: &str) -> Option<SubagentInfo> {
        self.registry
            .lock()
            .unwrap()
            .get(id)
            .map(|h| h.info.clone())
    }

    /// All subagents, oldest first.
    pub fn list(&self) -> Vec<SubagentInfo> {
        let mut infos: Vec<SubagentInfo> = self
            .registry
            .lock()
            .unwrap()
            .values()
            .map(|handle| handle.info.clone())
            .collect();
        infos.sort_by_key(|info| info.started_ms);
        infos
    }

    /// Read a subagent's persisted transcript (empty when there is no store / no file yet).
    pub fn load_transcript(&self, id: &str) -> Vec<Message> {
        self.store()
            .and_then(|store| store.load_transcript(id).ok())
            .unwrap_or_default()
    }

    /// Cancel a running subagent (`AbortHandle::abort`). Transitions to `Cancelled`, records it and
    /// notifies subscribers. Cancelling an already-finished subagent is a no-op (keeps its status).
    pub fn cancel(&self, id: &str) -> Result<(), SubagentError> {
        let (abort, transitioned, info) = {
            let mut registry = self.registry.lock().unwrap();
            let handle = registry
                .get_mut(id)
                .ok_or_else(|| SubagentError::NotFound(id.to_owned()))?;
            let transitioned = handle.info.status == SubagentStatus::Running;
            if transitioned {
                handle.info.status = SubagentStatus::Cancelled;
                handle.info.finished_ms = Some(now_ms());
            }
            (handle.abort.clone(), transitioned, handle.info.clone())
        };
        if let Some(abort) = abort {
            abort.abort();
        }
        if transitioned {
            self.record_finished(&info);
            self.emit(id, SubagentEvent::Status(SubagentStatus::Cancelled));
        }
        Ok(())
    }

    /// Called from the spawned task once the child's stream ends. Does not overwrite a `Cancelled`
    /// status (a race between `cancel` and a just-finished child keeps `Cancelled`).
    fn finish(&self, id: &str, outcome: Result<RunResult, AgentError>) {
        let info = {
            let mut registry = self.registry.lock().unwrap();
            let Some(handle) = registry.get_mut(id) else {
                return;
            };
            if handle.info.status == SubagentStatus::Cancelled {
                return;
            }
            handle.info.finished_ms = Some(now_ms());
            match outcome {
                Ok(result) => {
                    handle.info.status = SubagentStatus::Completed;
                    handle.info.result = Some(SubagentResult {
                        summary: summarize(&result.messages),
                        stop_reason: format!("{:?}", result.stop_reason),
                        usage: result.usage,
                    });
                }
                Err(error) => {
                    handle.info.status = SubagentStatus::Failed;
                    handle.info.result = Some(SubagentResult {
                        summary: format!("subagent failed: {error}"),
                        stop_reason: "Failed".into(),
                        usage: Usage::default(),
                    });
                }
            }
            handle.info.clone()
        };
        self.record_finished(&info);
        self.emit(id, SubagentEvent::Status(info.status));
    }

    /// Push an event to whoever is subscribed to `id` (dropped if nobody is).
    fn emit(&self, id: &str, event: SubagentEvent) {
        let sender = self.events.lock().unwrap().get(id).cloned();
        if let Some(sender) = sender {
            let _ = sender.send(event);
        }
    }

    /// Append the `started` index record (best-effort).
    fn record_started(
        &self,
        id: &str,
        kind: SubagentKind,
        name: &str,
        task: &str,
        started_ms: u64,
    ) {
        let Some(store) = self.store() else {
            return;
        };
        let _ = store.append_index(&IndexRecord::Started {
            id: id.to_owned(),
            kind: kind.as_str().to_owned(),
            name: name.to_owned(),
            task: task.to_owned(),
            parent: Some(store.session().to_owned()),
            started_ms,
        });
    }

    /// Append the `finished` index record (best-effort).
    fn record_finished(&self, info: &SubagentInfo) {
        let Some(store) = self.store() else {
            return;
        };
        let (summary, stop_reason, usage) = match &info.result {
            Some(result) => (
                result.summary.clone(),
                result.stop_reason.clone(),
                result.usage,
            ),
            None => (
                String::new(),
                format!("{:?}", info.status),
                Usage::default(),
            ),
        };
        let _ = store.append_index(&IndexRecord::Finished {
            id: info.id.clone(),
            status: info.status.as_str().to_owned(),
            stop_reason,
            finished_ms: info.finished_ms.unwrap_or_else(now_ms),
            summary,
            usage,
        });
    }

    /// Seed the registry from a restored `index.jsonl`. A record left `running` means the process
    /// exited mid-flight: it is reported as `failed` (no checkpoint-resume, decision 9).
    fn restore_from_index(&self) {
        let Some(store) = self.store() else {
            return;
        };
        let Ok(records) = store.load_index() else {
            return;
        };
        {
            let mut registry = self.registry.lock().unwrap();
            for record in records {
                match record {
                    IndexRecord::Started {
                        id,
                        kind,
                        name,
                        task,
                        started_ms,
                        ..
                    } => {
                        let kind = SubagentKind::from_str(&kind).unwrap_or(SubagentKind::Spawn);
                        let name = resolve_name((!name.is_empty()).then_some(name), &task);
                        registry
                            .entry(id.clone())
                            .or_insert_with(|| SubagentHandle {
                                info: SubagentInfo {
                                    id,
                                    kind,
                                    name,
                                    task,
                                    status: SubagentStatus::Running,
                                    started_ms,
                                    finished_ms: None,
                                    result: None,
                                },
                                join: None,
                                abort: None,
                            });
                    }
                    IndexRecord::Finished {
                        id,
                        status,
                        stop_reason,
                        finished_ms,
                        summary,
                        usage,
                    } => {
                        if let Some(handle) = registry.get_mut(&id) {
                            handle.info.status =
                                SubagentStatus::from_str(&status).unwrap_or(SubagentStatus::Failed);
                            handle.info.finished_ms = Some(finished_ms);
                            handle.info.result = Some(SubagentResult {
                                summary,
                                stop_reason,
                                usage,
                            });
                        }
                    }
                }
            }
            for handle in registry.values_mut() {
                if handle.info.status == SubagentStatus::Running {
                    handle.info.status = SubagentStatus::Failed;
                    handle.info.result = Some(SubagentResult {
                        summary: "subagent was interrupted (the process exited before it finished)"
                            .into(),
                        stop_reason: "Interrupted".into(),
                        usage: Usage::default(),
                    });
                }
            }
        }
    }
}

/// The fork snapshot: the parent view **with the leading `System` stripped** (decision 2 -- the
/// child's `Agent::new` regenerates its own system and inserts it at index 0; keeping the parent's
/// would leave two systems, the first stale), and with a trailing incomplete assistant tool-call
/// turn dropped (a fork must not inherit a half-finished tool group).
fn fork_snapshot(view: &[Message]) -> Vec<Message> {
    let mut messages: Vec<Message> = view
        .iter()
        .skip_while(|message| matches!(message, Message::System { .. }))
        .cloned()
        .collect();
    while matches!(
        messages.last(),
        Some(Message::Assistant { tool_calls, .. }) if !tool_calls.is_empty()
    ) {
        messages.pop();
    }
    messages
}

/// The child's final conclusion: the last non-empty assistant body.
fn summarize(messages: &[Message]) -> String {
    let mut last = String::new();
    for message in messages {
        if let Message::Assistant {
            content: Some(content),
            ..
        } = message
            && !content.trim().is_empty()
        {
            last.clone_from(content);
        }
    }
    last
}

/// Resolve a subagent's display name: the model-supplied `name` when it is non-empty, otherwise
/// the first line of `task` (trimmed, capped). This keeps older logs (no `name`) legible.
fn resolve_name(name: Option<String>, task: &str) -> String {
    if let Some(name) = name {
        let name = name.trim();
        if !name.is_empty() {
            return truncate_chars(name, MAX_NAME_CHARS);
        }
    }
    let first_line = task.lines().next().unwrap_or("").trim();
    let fallback = if first_line.is_empty() {
        "subagent"
    } else {
        first_line
    };
    truncate_chars(fallback, MAX_NAME_CHARS)
}

/// Cap a display name by chars (not bytes) so a multi-byte boundary is never split.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
