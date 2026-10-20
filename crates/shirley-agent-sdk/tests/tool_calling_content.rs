//!   without a hard abort.   suppressed;   protocol source text, and feeds the tool result back as a user message;   zero runtime changes;   injected at the front of the system prompt; endpoint and runs the whole runtime". This file fills that gap: real tool call plus a final answer. **End-to-end** contract for the content tool-calling protocol: a model endpoint that only
//! returns text (never native `tool_calls`), through the full `ReAct` loop, must be drivable into a
//!
//! Previously this path was covered only by unit tests inside the `tool_calling.rs` module (the
//! parser / encoder each correct in isolation), with no integration test that "mocks a plain-text
//!
//! - Encode side: the request **no longer has** native `tools` / `tool_choice`; the tool catalog is
//! - Decode side: the `<tool>` block in the model's text is parsed into `tool_calls`, executable with
//! - Replay side: the next request deterministically re-renders the historical assistant into the
//! - Streaming: `<description>` -> `ContentDelta`, `<thinking>` -> `ReasoningDelta`; the tool block is
//! - Fault tolerance: a malformed tool block is fed back through the reserved tool `__protocol__`,
//!
//! No additions to the public contract: all assertions land on `AgentEvent` / `RunResult` / the request body.

use futures::StreamExt;
use serde_json::{Value, json};
use shirley_agent_sdk::{
    Agent, AgentEvent, Message, ModelConfig, ModelProtocol, StopReason, ToolCallingMode, tool,
};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// The most ordinary tool: returns the input text with a prefix, making the fed-back result easy to assert.
#[tool(description = "return the input text with a prefix added")]
async fn echo(
    #[param(description = "text to echo")] text: String,
) -> Result<String, shirley_agent_sdk::ToolError> {
    Ok(format!("echo: {text}"))
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl Response {
    fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.into(),
        }
    }

    fn sse(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            body: body.into(),
        }
    }
}

/// A local HTTP fixture: records each request body and returns responses in order (the last one repeats).
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
        let captured = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
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
                let header = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    response.content_type,
                    response.body.len()
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
            .model_config(
                ModelConfig::builder()
                    .protocol(ModelProtocol::ChatCompletions)
                    .base_url(&self.url)
                    .model("fixture")
                    .stream(streaming)
                    // The star of this file: declare the text tool protocol.
                    .tool_calling(ToolCallingMode::Tagged)
                    .build(),
            )
            .system_prompt("You are a test assistant")
            .tools({
                let mut tools = shirley_agent_sdk::ToolManager::new();
                tools.register(echo::tool()).unwrap();
                tools
            })
            .build()
            .unwrap()
    }

    fn request(&self, index: usize) -> Value {
        self.requests.lock().unwrap()[index].clone()
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

/// A non-streaming assistant text response (`finish_reason: stop`).
fn text_response(content: &str) -> Response {
    Response::json(
        json!({
            "choices": [{
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4 }
        })
        .to_string(),
    )
}

/// Streaming: split a text into several `delta.content` pieces, then give `finish_reason: stop`.
fn sse_text(pieces: &[&str]) -> Response {
    let mut body = String::new();
    for piece in pieces {
        let _ = write!(
            body,
            "data: {}\n\n",
            json!({"choices": [{"index": 0, "delta": {"content": piece}, "finish_reason": null}]})
        );
    }
    let _ = write!(
        body,
        "data: {}\n\n",
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
    );
    body.push_str("data: [DONE]\n\n");
    Response::sse(body)
}

/// Encode side: in content mode the request body must no longer have native tool fields; the tool
/// catalog goes at the front of the system prompt.
#[tokio::test]
async fn request_swaps_native_tools_for_a_system_catalog() {
    let server = Server::new(vec![text_response("<description>done</description>")]);
    let mut agent = server.agent(false);
    agent.run("hi").await.unwrap();

    let request = server.request(0);
    assert_eq!(
        request["tools"],
        json!([]),
        "content mode must not send native tools: {request}"
    );
    assert!(
        request.get("tool_choice").is_none(),
        "content mode must not send tool_choice: {request}"
    );
    assert!(
        request.get("parallel_tool_calls").is_none(),
        "content mode should remove parallel_tool_calls: {request}"
    );
    let system = request["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("# Tool-calling protocol"),
        "the tool-protocol contract is missing: {system}"
    );
    assert!(
        system.contains("# Available tools"),
        "the tool catalog is missing: {system}"
    );
    assert!(
        system.contains("echo"),
        "the tool name is missing: {system}"
    );
}

/// Full chain (non-streaming): the model initiates a call via the text protocol -> the runtime
/// executes -> feedback -> final answer.
#[tokio::test]
async fn content_text_is_parsed_into_a_real_tool_call_and_loops_to_completion() {
    let server = Server::new(vec![
        text_response(
            "<thinking>echo first</thinking><description>calling echo</description>\
             <tool><name>echo</name><params>{\"text\":\"hi\"}</params></tool>",
        ),
        text_response("<description>done</description>"),
    ]);
    let mut agent = server.agent(false);
    let result = agent.run("please echo hi").await.unwrap();

    // Two rounds of model calls (the first issues the tool, the second wraps up).
    assert_eq!(server.count(), 2, "there should be exactly two model calls");

    // The final answer is plain text with the XML shell stripped off.
    let last = result.messages.last().unwrap();
    match last {
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            assert_eq!(content.as_deref(), Some("done"));
            assert!(tool_calls.is_empty());
        }
        other => panic!("the last round should be the assistant wrapping up, got: {other:?}"),
    }
    assert!(
        matches!(result.stop_reason, StopReason::Completed),
        "should wrap up normally, got: {:?}",
        result.stop_reason
    );

    // Second request: the historical assistant is deterministically re-rendered into the protocol
    // source text, and the tool result is fed back as user text.
    let second = server.request(1);
    let messages = second["messages"].as_array().unwrap();
    let serialized = serde_json::Value::Array(messages.clone()).to_string();
    assert!(
        serialized.contains("<name>echo</name>"),
        "the historical assistant was not replayed as protocol source text: {serialized}"
    );
    // The last user message should be the tool-result feedback: protocol feedback header + tool output.
    let result_user = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .unwrap();
    let content = result_user["content"].as_str().unwrap();
    assert!(
        content.contains(r#"{"tool":"echo"}"#),
        "the tool result is missing the protocol feedback header: {content}"
    );
    assert!(
        content.contains("echo: hi"),
        "the tool result was not fed back: {content}"
    );
}

/// Event layer (non-streaming): consuming `AgentEvent` should show `echo`'s `ToolStarted` / `ToolFinished`.
#[tokio::test]
async fn content_mode_emits_the_same_agent_events_as_native() {
    let server = Server::new(vec![
        text_response(
            "<description>calling echo</description>\
             <tool><name>echo</name><params>{\"text\":\"hi\"}</params></tool>",
        ),
        text_response("<description>done</description>"),
    ]);
    let mut agent = server.agent(false);

    let mut saw_start = false;
    let mut saw_finish = false;
    let mut last_assistant = None;
    let mut stream = agent.run_stream("please echo hi");
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            AgentEvent::ToolStarted { name, .. } => {
                assert_eq!(name, "echo");
                saw_start = true;
            }
            AgentEvent::ToolFinished {
                name, ok, output, ..
            } => {
                assert_eq!(name, "echo");
                assert!(ok);
                // The tool returns a `String`; the runtime stores it via `Value::to_string()` -> wrapped in JSON quotes.
                assert_eq!(output, "\"echo: hi\"");
                saw_finish = true;
            }
            // There is no ContentDelta in non-streaming mode (that is streaming-only); the body is stored via MessageAdded.
            AgentEvent::MessageAdded(Message::Assistant { content, .. }) => {
                last_assistant = content;
            }
            _ => {}
        }
    }
    assert!(saw_start, "missing ToolStarted");
    assert!(saw_finish, "missing ToolFinished");
    assert_eq!(
        last_assistant.as_deref(),
        Some("done"),
        "the final assistant body should be the shell-stripped plain text"
    );
}

/// Streaming: `<thinking>` -> `ReasoningDelta`, `<description>` -> `ContentDelta`, tool block suppressed.
#[tokio::test]
async fn streaming_hides_the_tool_block_and_surfaces_thinking_and_text() {
    let server = Server::new(vec![
        sse_text(&[
            "<thinking>let me think</thinking>",
            "<description>pro",
            "cessing</description>",
            "<tool><name>echo</name>",
            "<params>{\"text\":\"hi\"}</params></tool>",
        ]),
        sse_text(&["<description>ok</description>"]),
    ]);
    let mut agent = server.agent(true);

    let mut reasoning = String::new();
    let mut content = String::new();
    let mut saw_finish = false;
    let mut stream = agent.run_stream("please echo hi");
    while let Some(event) = stream.next().await {
        match event.unwrap() {
            AgentEvent::ReasoningDelta(text) => reasoning.push_str(&text),
            AgentEvent::ContentDelta(text) => content.push_str(&text),
            AgentEvent::ToolFinished { name, ok, .. } => {
                assert_eq!(name, "echo");
                assert!(ok);
                saw_finish = true;
            }
            _ => {}
        }
    }
    assert_eq!(
        reasoning, "let me think",
        "thinking should map to a reasoning delta"
    );
    assert_eq!(
        content, "processingok",
        "the body should be the concatenation of the two rounds of description"
    );
    assert!(
        !content.contains("<tool>") && !content.contains("<name>"),
        "the tool block must not leak into ContentDelta: {content}"
    );
    assert!(saw_finish, "streaming should also execute the tool");
}

/// Fault tolerance: a malformed tool block is fed back through the reserved tool `__protocol__`,
/// without a hard abort (zero runtime changes).
#[tokio::test]
async fn malformed_tool_block_is_recovered_through_the_protocol_tool() {
    let server = Server::new(vec![
        // Missing `<name>` -> parse failure -> synthesize a `__protocol__` call.
        text_response("<tool><params>{\"text\":\"hi\"}</params></tool>"),
        text_response("<description>fixed</description>"),
    ]);
    let mut agent = server.agent(false);

    let mut saw_protocol_tool = false;
    let mut stream = agent.run_stream("please echo hi");
    while let Some(event) = stream.next().await {
        if let AgentEvent::ToolStarted { name, .. } = event.unwrap()
            && name == "__protocol__"
        {
            saw_protocol_tool = true;
        }
    }
    assert!(
        saw_protocol_tool,
        "a parse failure should be fed back as a __protocol__ call"
    );
    assert_eq!(
        server.count(),
        2,
        "after feedback it should continue to the next round, not abort"
    );

    let second = server.request(1);
    let serialized = second["messages"].to_string();
    assert!(
        serialized.contains("The tool call could not be parsed"),
        "the feedback should carry a correctable error explanation: {serialized}"
    );
}
