//! Runtime contract for network retries and context retention after interruption; no UI involved.
use futures::StreamExt;
use serde_json::{Value, json};
use shirley_agent_sdk::{
    Agent, AgentEvent, ContextProvider, ErrorKind, Message, ModelConfig, ModelProtocol, SdkError,
    StopReason, Tool, ToolCall, ToolContext, ToolDefinition, ToolFuture, ToolManager,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

struct Response {
    status: u16,
    body: String,
    truncated: bool,
}

impl Response {
    fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            truncated: false,
        }
    }

    fn truncated(body: &str) -> Self {
        Self {
            status: 200,
            body: body.into(),
            truncated: true,
        }
    }
}

/// A local HTTP fixture: records full requests, returns responses in order, and repeats the last response.
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn new(responses: Vec<Response>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            'connections: while !stopping.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("accept: {error}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let (header_end, body_length) = loop {
                    let mut chunk = [0; 4096];
                    let read = match socket.read(&mut chunk) {
                        Ok(0) | Err(_) => continue 'connections,
                        Ok(read) => read,
                    };
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        break (end + 4, length);
                    }
                };
                while bytes.len() < header_end + body_length {
                    let mut chunk = [0; 4096];
                    let read = socket.read(&mut chunk).unwrap();
                    assert!(read > 0, "request ended before body");
                    bytes.extend_from_slice(&chunk[..read]);
                }
                let request =
                    serde_json::from_slice(&bytes[header_end..header_end + body_length]).unwrap();
                let index = {
                    let mut requests = captured.lock().unwrap();
                    let index = requests.len();
                    requests.push(request);
                    index
                };
                let response = &responses[index.min(responses.len() - 1)];
                let length = response.body.len() + if response.truncated { 100 } else { 0 };
                let header = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n",
                    response.status
                );
                let _ = socket.write_all(header.as_bytes());
                let _ = socket.write_all(response.body.as_bytes());
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn agent(&self, streaming: bool) -> Agent {
        Agent::builder()
            .model_config(config(&self.url, streaming))
            .build()
            .unwrap()
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

fn config(url: &str, streaming: bool) -> ModelConfig {
    ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url(url)
        .model("fixture")
        .stream(streaming)
        .build()
}

fn success() -> Response {
    Response::new(200, json!({
        "choices": [{"message": {"role": "assistant", "content": "done"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
    }).to_string())
}

fn sse(delta: Value, finished: bool) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices": [{"index": 0, "delta": delta, "finish_reason": if finished { Some("stop") } else { None }}]})
    )
}

#[tokio::test]
async fn succeeds_on_fifth_retry_without_replaying_the_user_turn() {
    let mut responses: Vec<_> = [429, 500, 502, 503, 429]
        .into_iter()
        .map(|status| Response::new(status, "busy"))
        .collect();
    responses.push(success());
    let server = Server::new(responses);
    let mut agent = server.agent(false);
    agent.run("hello").await.unwrap();
    assert_eq!(server.count(), 6);
    let requests = server.requests.lock().unwrap();
    assert!(requests.iter().all(|request| request == &requests[0]));
    assert_eq!(
        agent
            .messages()
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn stops_after_five_retries_and_keeps_the_last_error() {
    let server = Server::new(vec![Response::new(429, "rate limit")]);
    let error = server.agent(false).run("hello").await.unwrap_err();
    assert_eq!(server.count(), 6);
    assert_eq!(error.kind(), ErrorKind::RateLimited);
    assert!(error.to_string().contains("rate limit"));
}

#[tokio::test]
async fn permanent_http_and_decode_errors_are_not_retried() {
    for status in [400, 401, 403] {
        // Even if reading the error body fails, the decision must still be based on the original HTTP status.
        let mut response = Response::new(status, "bad request");
        response.truncated = true;
        let server = Server::new(vec![response]);
        let error = server.agent(false).run("hello").await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::BadRequest);
        assert_eq!(server.count(), 1);
    }
    let server = Server::new(vec![Response::new(200, "invalid JSON")]);
    assert_eq!(
        server.agent(false).run("hello").await.unwrap_err().kind(),
        ErrorKind::Internal
    );
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn retries_response_body_transport_errors_before_any_delta() {
    let server = Server::new(vec![Response::truncated("{"), success()]);
    server.agent(false).run("hello").await.unwrap();
    assert_eq!(server.count(), 2);

    let done = sse(json!({"content": "done"}), true);
    let server = Server::new(vec![
        Response::truncated(": keepalive\n\n"),
        Response::new(200, done),
    ]);
    server.agent(true).run("hello").await.unwrap();
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn transport_failure_after_delta_is_not_retried_and_partial_is_next_turn_context() {
    let partial = sse(
        json!({"content": "half", "reasoning_content": "reason"}),
        false,
    );
    let done = sse(json!({"content": "done"}), true);
    let server = Server::new(vec![
        Response::truncated(&partial),
        Response::new(200, done),
    ]);
    let mut agent = server.agent(true);
    let mut stream = agent.run_stream("first");
    let error = loop {
        if let Err(error) = stream.next().await.expect("transport failure expected") {
            break error;
        }
    };
    assert_eq!(error.kind(), ErrorKind::Transport);
    drop(stream);
    assert_eq!(server.count(), 1);
    let saved = agent.finish_interrupted_turn();
    assert!(
        matches!(&saved[..], [Message::Assistant { content: Some(content), reasoning_content: Some(reasoning), .. }] if content == "half" && reasoning == "reason")
    );
    assert!(agent.finish_interrupted_turn().is_empty());
    agent.run("continue").await.unwrap();
    let requests = server.requests.lock().unwrap();
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages[0]["content"], "first");
    assert_eq!(messages[1]["content"], "half");
    assert_eq!(messages[1]["reasoning_content"], "reason");
    assert_eq!(messages[2]["content"], "continue");
}

#[tokio::test]
async fn dropping_stream_keeps_user_and_partial_response_once() {
    let server = Server::new(vec![Response::new(
        200,
        sse(json!({"content": "half"}), true),
    )]);
    let mut agent = server.agent(true);
    let mut stream = agent.run_stream("first");
    while !matches!(
        stream.next().await.unwrap().unwrap(),
        AgentEvent::ContentDelta(_)
    ) {}
    drop(stream);
    assert_eq!(agent.finish_interrupted_turn().len(), 1);
    assert!(
        matches!(agent.messages(), [Message::User { content: user }, Message::Assistant { content: Some(text), .. }] if user == "first" && text == "half")
    );
    assert!(agent.finish_interrupted_turn().is_empty());
}

#[test]
fn dropping_before_poll_keeps_the_submitted_user_message() {
    let mut agent = Agent::builder()
        .model_config(config("http://127.0.0.1:1", true))
        .build()
        .unwrap();
    drop(agent.run_stream("first"));
    assert!(
        matches!(&agent.finish_interrupted_turn()[..], [Message::User { content }] if content == "first")
    );
    assert_eq!(agent.messages().len(), 1);
    assert!(agent.finish_interrupted_turn().is_empty());
}

#[test]
fn interrupted_tools_get_paired_results_without_overwriting_finished_tools() {
    let mut agent = Agent::builder()
        .model_config(config("http://127.0.0.1:1", false))
        .messages(vec![
            Message::User {
                content: "first".into(),
            },
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: ["a", "b"]
                    .into_iter()
                    .map(|id| ToolCall {
                        id: id.into(),
                        name: "fixture".into(),
                        arguments: "{}".into(),
                    })
                    .collect(),
            },
            Message::Tool {
                tool_call_id: "a".into(),
                content: Some("finished".into()),
            },
        ])
        .build()
        .unwrap();
    let saved = agent.finish_interrupted_turn();
    assert!(matches!(&saved[..], [Message::Tool { tool_call_id, .. }] if tool_call_id == "b"));
    assert!(
        matches!(&agent.messages()[2], Message::Tool { content: Some(content), .. } if content == "finished")
    );
    assert!(agent.finish_interrupted_turn().is_empty());
}

struct EvidenceTool(ToolDefinition);

impl Tool for EvidenceTool {
    fn definition(&self) -> &ToolDefinition {
        &self.0
    }

    fn invoke(&self, _: Value, _: ToolContext) -> ToolFuture<'_> {
        Box::pin(async { Ok(json!("execution evidence ".repeat(250))) })
    }
}

struct Ledger;
impl ContextProvider for Ledger {
    fn context(&self) -> Option<String> {
        Some("normal-request-context".into())
    }
    fn compaction_context(&self) -> Option<String> {
        Some("<task_state>confirmed: running agent model name is empty</task_state>".into())
    }
}

fn reply(content: Option<&str>, call_id: Option<&str>, input_tokens: u64) -> Response {
    let mut message = json!({"role": "assistant", "content": content});
    if let Some(id) = call_id {
        message["tool_calls"] = json!([{
            "id": id, "type": "function", "function": {"name": "evidence", "arguments": "{}"}
        }]);
    }
    Response::new(200, json!({
        "choices": [{"message": message, "finish_reason": if call_id.is_some() { "tool_calls" } else { "stop" }}],
        "usage": {"prompt_tokens": input_tokens, "completion_tokens": 1, "total_tokens": input_tokens + 1}
    }).to_string())
}

fn evidence_tools() -> ToolManager {
    let mut tools = ToolManager::new();
    tools
        .register(EvidenceTool(ToolDefinition {
            name: "evidence".into(),
            description: "Read fixture evidence".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }))
        .unwrap();
    tools
}

/// 20261013-142807 badcase: successive compactions within the same task; capture the real HTTP request
/// and verify that prior conclusions, tool evidence, and the ledger all make it into the next summary, not just that the XML tags are checked.
#[tokio::test]
async fn successive_compactions_inherit_findings_evidence_and_ledger() {
    let server = Server::new(vec![
        reply(None, Some("run-a"), 8000),
        reply(Some("summary-one: model name loss confirmed"), None, 0),
        reply(None, Some("run-b"), 8000),
        reply(Some("summary-two: model name loss confirmed"), None, 0),
        reply(Some("done"), None, 5),
    ]);
    let mut model = config(&server.url, false);
    model.context_window_tokens = Some(10_000);
    let history = vec![
        Message::ContextSummary {
            content: "prior-confirmed-finding: active_agent is absent while running".into(),
        },
        Message::User {
            content: "fix session switch; preserve user constraints".into(),
        },
        Message::Assistant {
            content: None,
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "history".into(),
                name: "evidence".into(),
                arguments: "{}".into(),
            }],
        },
        Message::Tool {
            tool_call_id: "history".into(),
            content: Some(format!(
                "modelName returned empty; {}",
                "code evidence ".repeat(1000)
            )),
        },
    ];
    let mut agent = Agent::builder()
        .model_config(model)
        .messages(history)
        .compression_instruction("Preserve findings and evidence")
        .context_provider(Arc::new(Ledger) as Arc<dyn ContextProvider>)
        .tools(evidence_tools())
        .max_steps(3)
        .build()
        .unwrap();
    let result = agent.run("continue diagnosis").await.unwrap();
    assert!(matches!(result.stop_reason, StopReason::Completed));
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    let first_summary = requests[1]["messages"].to_string();
    assert!(first_summary.contains("prior-confirmed-finding"));
    assert!(first_summary.contains("modelName returned empty"));
    assert!(first_summary.contains("<task_state>confirmed:"));
    assert!(first_summary.contains("continue diagnosis"));
    assert!(first_summary.contains("preserve user constraints"));
    assert!(!first_summary.contains("normal-request-context"));
    assert!(!first_summary.contains("tool result omitted"));
    let second_summary = requests[3]["messages"].to_string();
    assert!(second_summary.contains("summary-one: model name loss confirmed"));
    assert!(second_summary.contains("execution evidence"));
    assert!(second_summary.contains("<task_state>confirmed:"));
    let execution = requests[4]["messages"].to_string();
    assert!(execution.contains("summary-two: model name loss confirmed"));
    assert!(!execution.contains("summary-one: model name loss confirmed"));
    assert!(execution.contains("normal-request-context"));
    assert_eq!(
        agent
            .messages()
            .iter()
            .filter(|m| matches!(m, Message::ContextSummary { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn endless_tool_calls_stop_at_budget_with_complete_tool_results_and_can_resume() {
    let server = Server::new(vec![reply(None, Some("repeated"), 3)]);
    let mut agent = Agent::builder()
        .model_config(config(&server.url, false))
        .tools(evidence_tools())
        .max_steps(2)
        .build()
        .unwrap();
    for task in ["investigate", "continue"] {
        let result = agent.run(task).await.unwrap();
        assert!(matches!(result.stop_reason, StopReason::MaxStepsReached));
        assert_eq!(
            result
                .messages
                .iter()
                .filter(|m| matches!(m, Message::Tool { .. }))
                .count(),
            2
        );
        assert!(
            matches!(result.messages.last(), Some(Message::Assistant { content: Some(text), tool_calls, .. }) if text.contains("not yet complete") && tool_calls.is_empty())
        );
        assert!(agent.finish_interrupted_turn().is_empty());
    }
    assert_eq!(server.count(), 4);
}

#[test]
fn rejects_zero_execution_budget() {
    assert!(
        Agent::builder()
            .model_config(config("http://127.0.0.1:1", false))
            .max_steps(0)
            .build()
            .is_err()
    );
}

#[tokio::test]
async fn compaction_does_not_reset_execution_budget() {
    let server = Server::new(vec![
        reply(None, Some("first"), 8000),
        reply(Some("confirmed finding"), None, 0),
        reply(None, Some("second"), 8000),
    ]);
    let mut model = config(&server.url, false);
    model.context_window_tokens = Some(10_000);
    let mut agent = Agent::builder()
        .model_config(model)
        .compression_instruction("Preserve progress")
        .messages(vec![Message::User {
            content: "prior task ".repeat(400),
        }])
        .tools(evidence_tools())
        .max_steps(2)
        .build()
        .unwrap();
    let result = agent.run("diagnose").await.unwrap();
    assert!(matches!(result.stop_reason, StopReason::MaxStepsReached));
    assert_eq!(
        server.count(),
        3,
        "two execution requests plus one compaction"
    );
    assert!(
        agent
            .messages()
            .iter()
            .any(|m| matches!(m, Message::ContextSummary { .. }))
    );
    assert!(
        matches!(agent.messages().iter().rev().nth(1), Some(Message::Tool { tool_call_id, .. }) if tool_call_id == "second")
    );
}

#[tokio::test]
async fn provider_rejection_keeps_original_context_without_local_estimate_gate() {
    let server = Server::new(vec![
        reply(Some("investigating"), None, 8000),
        Response::new(400, "provider context limit exceeded"),
    ]);
    let mut model = config(&server.url, false);
    model.context_window_tokens = Some(10_000);
    let mut agent = Agent::builder()
        .model_config(model)
        .compression_instruction("Preserve progress")
        .messages(vec![Message::User {
            content: "constraint ".repeat(5000),
        }])
        .build()
        .unwrap();
    agent.run("diagnose").await.unwrap();
    let original = serde_json::to_value(agent.messages()).unwrap();
    let error = agent.run("continue").await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("provider context limit exceeded")
    );
    assert_eq!(serde_json::to_value(agent.messages()).unwrap(), original);
    assert_eq!(
        server.count(),
        2,
        "heuristic estimate is not a hard rejection gate"
    );
    assert!(
        matches!(&agent.finish_interrupted_turn()[..], [Message::User { content }] if content == "continue")
    );
}

#[tokio::test]
async fn restored_archive_compacts_only_latest_working_set() {
    let server = Server::new(vec![
        reply(None, Some("run"), 8000),
        reply(Some("inherited confirmed finding"), None, 0),
        reply(Some("done"), None, 5),
    ]);
    let mut model = config(&server.url, false);
    model.context_window_tokens = Some(10_000);
    let mut history = vec![
        Message::User {
            content: "obsolete archived task".into(),
        },
        Message::Assistant {
            content: Some("obsolete archived response".into()),
            reasoning_content: Some("obsolete archived reasoning ".repeat(100_000)),
            tool_calls: vec![],
        },
        Message::ContextSummary {
            content: "latest summary: running agent has no model name".into(),
        },
    ];
    for i in 0..3 {
        history.push(Message::Assistant {
            content: Some("read source".into()),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: format!("history-{i}"),
                name: "evidence".into(),
                arguments: "{}".into(),
            }],
        });
        history.push(Message::Tool {
            tool_call_id: format!("history-{i}"),
            content: Some("source evidence ".repeat(300)),
        });
    }
    let mut agent = Agent::builder()
        .model_config(model)
        .messages(history)
        .compression_instruction("Preserve findings")
        .tools(evidence_tools())
        .build()
        .unwrap();
    agent.run("continue current fix").await.unwrap();
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests.iter() {
        assert!(
            !request["messages"]
                .to_string()
                .contains("obsolete archived")
        );
    }
    let compaction = requests[1]["messages"].to_string();
    assert!(compaction.contains("latest summary: running agent has no model name"));
    assert!(compaction.contains("source evidence"));
    assert!(compaction.contains("continue current fix"));
}

#[tokio::test]
async fn over_budget_scratchpad_is_removed_before_tool_evidence() {
    let server = Server::new(vec![
        reply(None, Some("run"), 8000),
        reply(Some("confirmed finding"), None, 0),
        reply(Some("done"), None, 5),
    ]);
    let mut model = config(&server.url, false);
    model.context_window_tokens = Some(10_000);
    let history = vec![
        Message::User {
            content: "original task".into(),
        },
        Message::Assistant {
            content: Some("confirmed conclusion".into()),
            reasoning_content: Some("scratchpad ".repeat(20_000)),
            tool_calls: vec![ToolCall {
                id: "history".into(),
                name: "evidence".into(),
                arguments: "{}".into(),
            }],
        },
        Message::Tool {
            tool_call_id: "history".into(),
            content: Some("important evidence must remain".into()),
        },
        // Keep another complete tool group at the tail so the reasoning-bearing history group falls into the to-compress region.
        Message::Assistant {
            content: None,
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "recent".into(),
                name: "evidence".into(),
                arguments: "{}".into(),
            }],
        },
        Message::Tool {
            tool_call_id: "recent".into(),
            content: Some("recent result ".repeat(700)),
        },
    ];
    let mut agent = Agent::builder()
        .model_config(model)
        .messages(history)
        .compression_instruction("Preserve findings")
        .tools(evidence_tools())
        .build()
        .unwrap();
    agent.run("continue").await.unwrap();
    let requests = server.requests.lock().unwrap();
    let summary = requests[1]["messages"].as_array().unwrap();
    assert!(summary.iter().all(|m| m.get("reasoning_content").is_none()));
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("important evidence must remain")
    );
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("confirmed conclusion")
    );
}
