//! 网络重试与中断后上下文保留的运行时契约；不涉及 UI。
use futures::StreamExt;
use serde_json::{Value, json};
use shirley_agent_sdk::{
    Agent, AgentEvent, ErrorKind, Message, ModelConfig, ModelProtocol, SdkError, ToolCall,
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

/// 本地 HTTP fixture：记录完整请求，按序返回响应，最后一条响应可重复。
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
        // 即使错误体读取失败，也必须按原始 HTTP 状态判定。
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
