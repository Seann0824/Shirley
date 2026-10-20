//! The `subagent` tool: the parent model's only entry point to dispatch and manage subagents
//! (`docs/subagent.md` decisions 3/4).
//!
//! One tool with an `action` discriminator rather than several tools: **one stable tool name = one
//! stable prefix**, so the cache is not broken every time a new action is added (decision 4). The
//! six actions:
//! - `spawn` / `fork`: dispatch a child. **Blocking by default** (the tool future stays pending
//!   until the child finishes -- exactly the parent "sleep" and the runtime's `FuturesUnordered`
//!   concurrency); `detach: true` returns a handle immediately and the child keeps running.
//! - `wait`: block on an already-dispatched child (`id` omitted = wait for all running ones),
//!   bounded by `timeout_ms`; on timeout it returns `status: running` and the child keeps going.
//! - `list` / `status`: read-only observation.
//! - `cancel`: abort a running child (`AbortHandle::abort`).
//!
//! Hand-written `Tool` impl (not `#[tool]`): the state (`Arc<SubagentManager>`) is held by the
//! tool and shared with the session, exactly like [`crate::tools::TodoTool`].

use std::sync::Arc;
use std::time::Duration;

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, Usage};

use super::manager::{
    DEFAULT_WAIT_TIMEOUT_MS, MAX_WAIT_TIMEOUT_MS, SubagentError, SubagentInfo, SubagentKind,
    SubagentManager, SubagentStatus, WaitOutcome,
};

/// The `subagent` tool. Holds the manager (the session's subagent registry + factory).
pub struct SubagentTool {
    manager: Arc<SubagentManager>,
    definition: ToolDefinition,
}

impl SubagentTool {
    pub fn new(manager: Arc<SubagentManager>) -> Self {
        Self {
            manager,
            definition: ToolDefinition {
                name: "subagent".into(),
                description: "Dispatch and manage subagents -- self-contained helpers that run in \
                    an isolated context and report back a conclusion. `action: spawn` gives a \
                    clean child (only `task` is visible to it); `action: fork` gives the child a \
                    snapshot of this conversation. Both block until the child finishes and return \
                    its answer -- run several in one turn to work in parallel, or pass \
                    `detach: true` to keep working and collect the result later with `action: \
                    wait`. A child can never dispatch another child. `action: list` / `status` \
                    inspect subagents; `action: cancel` aborts one."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["spawn", "fork", "wait", "list", "status", "cancel"],
                            "description": "spawn = clean-context child; fork = child inherits a snapshot of this conversation; wait = block on a dispatched child; list = all subagents; status = one subagent; cancel = abort a running child."
                        },
                        "name": {
                            "type": "string",
                            "description": "spawn/fork only: a short human-readable label for this subagent (what it is doing), shown in the UI so the user can tell agents apart. Optional; defaults to the first line of `task`."
                        },
                        "task": {
                            "type": "string",
                            "description": "The sub-task for the child (required for spawn/fork). Self-contained: state the goal and what to report back."
                        },
                        "tools": {
                            "type": "array",
                            "description": "Optional allowlist of tool names the child may use. Defaults to the available tool set. `subagent` is never allowed.",
                            "items": { "type": "string" }
                        },
                        "deny_tools": {
                            "type": "array",
                            "description": "Optional tool names to subtract from the child's tool set.",
                            "items": { "type": "string" }
                        },
                        "id": {
                            "type": "string",
                            "description": "The subagent id (for wait/status/cancel; wait without an id waits for all running subagents)."
                        },
                        "detach": {
                            "type": "boolean",
                            "description": "spawn/fork only: true = dispatch to the background and return a handle immediately; false (default) = block until the child finishes."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "description": "wait only: how long to block before giving up (default 600000, max 3600000). On timeout the child keeps running."
                        }
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }),
            },
        }
    }
}

/// The tool arguments. `deny_unknown_fields` so a typo is reported, not silently ignored.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SubagentArgs {
    action: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    deny_tools: Option<Vec<String>>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    detach: Option<bool>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

impl Tool for SubagentTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        let manager = Arc::clone(&self.manager);
        Box::pin(async move {
            let args: SubagentArgs = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            match args.action.as_str() {
                "spawn" | "fork" => {
                    let kind = if args.action == "fork" {
                        SubagentKind::Fork
                    } else {
                        SubagentKind::Spawn
                    };
                    let task = args
                        .task
                        .clone()
                        .ok_or_else(|| ToolError::ArgumentsError("`task` is required".into()))?;
                    // depth = 0 for a top-level dispatch (the parent is always the top level).
                    let id = manager
                        .dispatch(
                            kind,
                            args.name.clone(),
                            task,
                            args.tools,
                            args.deny_tools,
                            0,
                        )
                        .map_err(|e| map_error(&e))?;
                    if args.detach.unwrap_or(false) {
                        // Non-blocking: hand back the handle; the child runs on.
                        let info = manager
                            .info(&id)
                            .ok_or_else(|| ToolError::ExecutionError("subagent vanished".into()))?;
                        return Ok(serde_json::json!({
                            "id": info.id,
                            "kind": info.kind.as_str(),
                            "name": info.name,
                            "status": info.status.as_str(),
                        }));
                    }
                    // Blocking: the parent "sleeps" on this await (decision 7).
                    manager.await_completion(&id).await;
                    let info = manager
                        .info(&id)
                        .ok_or_else(|| ToolError::ExecutionError("subagent vanished".into()))?;
                    Ok(info_json(&info))
                }
                "wait" => {
                    let timeout = resolve_timeout(&args)?;
                    if let Some(id) = args.id {
                        let outcome = manager
                            .wait(&id, Some(timeout))
                            .await
                            .map_err(|e| map_error(&e))?;
                        Ok(wait_json(&outcome, timeout))
                    } else {
                        // Wait for every currently-running subagent, bounded by the deadline.
                        let deadline = tokio::time::Instant::now() + timeout;
                        let ids: Vec<String> = manager
                            .list()
                            .iter()
                            .filter(|info| info.status == SubagentStatus::Running)
                            .map(|info| info.id.clone())
                            .collect();
                        for id in ids {
                            let remaining =
                                deadline.saturating_duration_since(tokio::time::Instant::now());
                            let _ = manager.wait(&id, Some(remaining)).await;
                        }
                        let agents: Vec<serde_json::Value> =
                            manager.list().iter().map(info_json).collect();
                        Ok(serde_json::json!({ "subagents": agents }))
                    }
                }
                "list" => {
                    let agents: Vec<serde_json::Value> =
                        manager.list().iter().map(info_json).collect();
                    Ok(serde_json::json!({ "subagents": agents }))
                }
                "status" => {
                    let id = args
                        .id
                        .ok_or_else(|| ToolError::ArgumentsError("`id` is required".into()))?;
                    let info = manager.info(&id).ok_or_else(|| {
                        ToolError::ArgumentsError(format!("no such subagent: {id}"))
                    })?;
                    Ok(info_json(&info))
                }
                "cancel" => {
                    let id = args
                        .id
                        .ok_or_else(|| ToolError::ArgumentsError("`id` is required".into()))?;
                    manager.cancel(&id).map_err(|e| map_error(&e))?;
                    let info = manager.info(&id);
                    Ok(match info {
                        Some(info) => serde_json::json!({
                            "id": info.id,
                            "status": info.status.as_str(),
                        }),
                        None => serde_json::json!({ "id": id, "status": "cancelled" }),
                    })
                }
                other => Err(ToolError::ArgumentsError(format!(
                    "unknown action `{other}` (expected spawn / fork / wait / list / status / cancel)"
                ))),
            }
        })
    }
}

/// Parse and cap `timeout_ms` (default [`DEFAULT_WAIT_TIMEOUT_MS`], hard cap [`MAX_WAIT_TIMEOUT_MS`]).
fn resolve_timeout(args: &SubagentArgs) -> Result<Duration, ToolError> {
    let ms = args.timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
    if ms == 0 || ms > MAX_WAIT_TIMEOUT_MS {
        return Err(ToolError::ArgumentsError(format!(
            "`timeout_ms` must be between 1 and {MAX_WAIT_TIMEOUT_MS}"
        )));
    }
    Ok(Duration::from_millis(ms))
}

/// A subagent snapshot as JSON. `summary` / `stop_reason` / `usage` appear only once finished.
fn info_json(info: &SubagentInfo) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": info.id,
        "kind": info.kind.as_str(),
        "name": info.name,
        "task": info.task,
        "status": info.status.as_str(),
        "elapsed_ms": info.elapsed_ms(),
    });
    if let Some(result) = &info.result {
        value["summary"] = serde_json::Value::String(result.summary.clone());
        value["stop_reason"] = serde_json::Value::String(result.stop_reason.clone());
        value["usage"] = usage_json(result.usage);
    }
    value
}

/// The `wait` result: the snapshot, or a "still running" marker on timeout.
fn wait_json(outcome: &WaitOutcome, timeout: Duration) -> serde_json::Value {
    if outcome.timed_out {
        serde_json::json!({
            "id": outcome.info.id,
            "status": outcome.info.status.as_str(),
            "waited_ms": timeout.as_millis() as u64,
            "timed_out": true,
        })
    } else {
        info_json(&outcome.info)
    }
}

fn usage_json(usage: Usage) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cached_input_tokens": usage.cached_input_tokens,
        "reasoning_tokens": usage.reasoning_tokens,
    })
}

/// Map a dispatch error to a `ToolError`. Unknown tool names / missing ids / the caps are
/// `ArgumentsError` (the model can fix its own call); a build failure is an execution failure.
fn map_error(error: &SubagentError) -> ToolError {
    match error {
        SubagentError::UnknownTool(_)
        | SubagentError::TaskRequired
        | SubagentError::TaskTooLong
        | SubagentError::DepthExceeded
        | SubagentError::ConcurrencyExceeded
        | SubagentError::NotFound(_) => ToolError::ArgumentsError(error.to_string()),
        SubagentError::Build(_) => ToolError::ExecutionError(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::test_support::test_factory_with_base;
    use serde_json::{Value, json};
    use shirley_agent_sdk::Message;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::thread;

    /// A minimal non-streaming `ChatCompletions` mock. Each request body is recorded, and the response
    /// is produced by `route` (so the parent's first call, the child's call, and the parent's final
    /// call can be told apart by inspecting the request).
    fn mock_model(
        route: impl Fn(&Value) -> Value + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut bytes = Vec::new();
                let (header_end, body_len) = loop {
                    let mut chunk = [0u8; 4096];
                    let read = match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break (0, 0),
                        Ok(read) => read,
                    };
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        break (end + 4, len);
                    }
                };
                if header_end == 0 {
                    continue;
                }
                while bytes.len() < header_end + body_len {
                    let mut chunk = [0u8; 4096];
                    let read = stream.read(&mut chunk).unwrap();
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..read]);
                }
                let body: Value =
                    serde_json::from_slice(&bytes[header_end..header_end + body_len]).unwrap();
                captured.lock().unwrap().push(body.clone());
                let response = route(&body);
                let text = response.to_string();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    text.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(text.as_bytes());
                let _ = stream.flush();
            }
        });
        (url, requests)
    }

    fn text_reply(content: &str) -> Value {
        json!({
            "id": "1",
            "choices": [{
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": content }
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })
    }

    fn tool_call_reply(action: &str, task: &str) -> Value {
        let arguments = json!({ "action": action, "task": task }).to_string();
        json!({
            "id": "1",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "subagent", "arguments": arguments }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })
    }

    fn request_has_subagent_tool(body: &Value) -> bool {
        body["tools"].as_array().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool["function"]["name"] == "subagent")
        })
    }

    fn roles(body: &Value) -> Vec<String> {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap().to_string())
            .collect()
    }

    /// spawn: the child gets a **clean** context `[system, user]` (only the task is visible), its
    /// tool table has no `subagent` (recursion guard), and the parent's `messages` grow by exactly
    /// one `tool_call` + one `tool_result` pair.
    #[tokio::test]
    async fn spawn_child_has_clean_context_and_parent_grows_by_one_tool_pair() {
        let (url, requests) = mock_model(|body| {
            let has_tool_result = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool");
            if has_tool_result {
                text_reply("parent-done")
            } else if request_has_subagent_tool(body) {
                tool_call_reply("spawn", "child task")
            } else {
                text_reply("child-answer")
            }
        });

        let root =
            std::env::temp_dir().join(format!("shirley_subagent_spawn_{}", std::process::id()));
        let factory = Arc::new(test_factory_with_base(root, &url, false));
        let mut built = factory.build_agent(Vec::new()).unwrap();

        let result = built.agent.run("parent task").await.unwrap();
        assert_eq!(
            result
                .messages
                .iter()
                .filter(|m| matches!(m, Message::Tool { .. }))
                .count(),
            1,
            "the parent's messages must grow by exactly one tool result"
        );

        let recorded = requests.lock().unwrap().clone();
        let child = recorded
            .iter()
            .find(|body| !request_has_subagent_tool(body))
            .expect("the child must have made a request");
        assert_eq!(
            roles(child),
            vec!["system", "user"],
            "a spawn child sees only its own system + the task"
        );
        assert_eq!(child["messages"][1]["content"], "child task");
        let child_tools: Vec<&str> = child["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert!(
            !child_tools.contains(&"subagent"),
            "the recursion guard must strip `subagent` from the child table: {child_tools:?}"
        );
    }

    /// fork: the child inherits the parent snapshot **with the leading `System` stripped**, so it
    /// has exactly one (its own regenerated) system plus the earlier turns plus the task.
    #[tokio::test]
    async fn fork_child_inherits_parent_snapshot_without_parent_system() {
        let (url, requests) = mock_model(|body| {
            let has_tool_result = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool");
            if has_tool_result {
                text_reply("parent-done")
            } else if request_has_subagent_tool(body) {
                tool_call_reply("fork", "child task")
            } else {
                text_reply("child-answer")
            }
        });

        let root =
            std::env::temp_dir().join(format!("shirley_subagent_fork_{}", std::process::id()));
        let factory = Arc::new(test_factory_with_base(root, &url, false));
        let mut built = factory.build_agent(Vec::new()).unwrap();

        // Seed the fork snapshot source (in the app the `Session` does this from `Agent::messages()`).
        let manager = built.subagents.clone().expect("top level has a manager");
        manager.set_parent_view(vec![
            Message::System {
                content: "parent-system".into(),
            },
            Message::User {
                content: "earlier question".into(),
            },
            Message::Assistant {
                content: Some("earlier answer".into()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ]);

        built.agent.run("parent task").await.unwrap();

        let recorded = requests.lock().unwrap().clone();
        let child = recorded
            .iter()
            .find(|body| !request_has_subagent_tool(body))
            .expect("the child must have made a request");
        let child_roles = roles(child);
        assert_eq!(
            child_roles,
            vec!["system", "user", "assistant", "user"],
            "fork child = [own system] + [earlier user/assistant] + [task]"
        );
        assert_eq!(child["messages"][1]["content"], "earlier question");
        assert_eq!(child["messages"][2]["content"], "earlier answer");
        assert_eq!(child["messages"][3]["content"], "child task");
        assert!(
            !child.to_string().contains("parent-system"),
            "the parent's leading system must be stripped"
        );
    }

    /// The tool dispatches through the manager and blocks: `list` observes the finished child.
    #[tokio::test]
    async fn list_reports_dispatched_subagents() {
        let (url, _requests) = mock_model(|_| text_reply("child-answer"));
        let root =
            std::env::temp_dir().join(format!("shirley_subagent_list_{}", std::process::id()));
        let factory = Arc::new(test_factory_with_base(root, &url, false));
        let built = factory.build_agent(Vec::new()).unwrap();
        let manager = built.subagents.clone().unwrap();
        let tool = SubagentTool::new(manager);
        let ctx = ToolContext::new();

        let spawned = tool
            .invoke(
                json!({ "action": "spawn", "task": "do a thing" }),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert_eq!(spawned["status"], "completed");
        assert_eq!(spawned["summary"], "child-answer");

        let listed = tool.invoke(json!({ "action": "list" }), ctx).await.unwrap();
        assert_eq!(listed["subagents"].as_array().unwrap().len(), 1);
    }

    /// `detach` returns a handle immediately, then `status` / `cancel` drive it: the child is
    /// aborted and the parent sees `cancelled` (P1).
    #[tokio::test]
    async fn detach_then_cancel_transitions_to_cancelled() {
        // The child sleeps well past the test, so it is guaranteed to still be running.
        let (url, _requests) = mock_model(|_| {
            std::thread::sleep(std::time::Duration::from_millis(3000));
            text_reply("child-answer")
        });
        let root =
            std::env::temp_dir().join(format!("shirley_subagent_cancel_{}", std::process::id()));
        let factory = Arc::new(test_factory_with_base(root, &url, false));
        let built = factory.build_agent(Vec::new()).unwrap();
        let manager = built.subagents.clone().unwrap();
        let tool = SubagentTool::new(manager);
        let ctx = ToolContext::new();

        let detached = tool
            .invoke(
                json!({ "action": "spawn", "task": "slow", "detach": true }),
                ctx.clone(),
            )
            .await
            .unwrap();
        let id = detached["id"].as_str().unwrap().to_string();
        assert_eq!(detached["status"], "running");

        // Cancel it and observe the state machine.
        let cancelled = tool
            .invoke(json!({ "action": "cancel", "id": id }), ctx.clone())
            .await
            .unwrap();
        assert_eq!(cancelled["status"], "cancelled");

        let status = tool
            .invoke(json!({ "action": "status", "id": id }), ctx)
            .await
            .unwrap();
        assert_eq!(status["status"], "cancelled");
    }

    /// `wait` on an already-finished subagent returns at once with `timed_out: false`.
    #[tokio::test]
    async fn wait_on_finished_subagent_returns_immediately() {
        let (url, _requests) = mock_model(|_| text_reply("child-answer"));
        let root =
            std::env::temp_dir().join(format!("shirley_subagent_wait_{}", std::process::id()));
        let factory = Arc::new(test_factory_with_base(root, &url, false));
        let built = factory.build_agent(Vec::new()).unwrap();
        let manager = built.subagents.clone().unwrap();
        let tool = SubagentTool::new(manager);
        let ctx = ToolContext::new();

        let detached = tool
            .invoke(
                json!({ "action": "spawn", "task": "quick", "detach": true }),
                ctx.clone(),
            )
            .await
            .unwrap();
        let id = detached["id"].as_str().unwrap().to_string();
        // Give the child a moment to finish.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let waited = tool
            .invoke(json!({ "action": "wait", "id": id }), ctx)
            .await
            .unwrap();
        assert_eq!(waited["status"], "completed");
        assert_eq!(waited["summary"], "child-answer");
        assert!(waited.get("timed_out").is_none());
    }

    /// `wait` times out on a still-running child and reports it (the child keeps going).
    #[tokio::test]
    async fn wait_times_out_while_child_still_running() {
        let (url, _requests) = mock_model(|_| {
            std::thread::sleep(std::time::Duration::from_millis(3000));
            text_reply("child-answer")
        });
        let root =
            std::env::temp_dir().join(format!("shirley_subagent_wait_to_{}", std::process::id()));
        let factory = Arc::new(test_factory_with_base(root, &url, false));
        let built = factory.build_agent(Vec::new()).unwrap();
        let manager = built.subagents.clone().unwrap();
        let tool = SubagentTool::new(manager);
        let ctx = ToolContext::new();

        let detached = tool
            .invoke(
                json!({ "action": "spawn", "task": "slow", "detach": true }),
                ctx.clone(),
            )
            .await
            .unwrap();
        let id = detached["id"].as_str().unwrap().to_string();

        let waited = tool
            .invoke(
                json!({ "action": "wait", "id": id, "timeout_ms": 150 }),
                ctx,
            )
            .await
            .unwrap();
        assert_eq!(waited["status"], "running");
        assert_eq!(waited["timed_out"], true);
    }
}
