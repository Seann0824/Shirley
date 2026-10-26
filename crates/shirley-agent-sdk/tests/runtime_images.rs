//! Image references must never reach a protocol encoder unresolved.
//!
//! The regression pinned here: the compaction request is assembled from `active_history()`, which
//! does not go through the request-view resolver. A `file_id` sitting in a compressed-away user
//! message was therefore handed straight to the encoder and the whole turn died with
//! `[encode failure]: ChatCompletions image source does not accept a file_id` -- exactly when the
//! context crossed the trigger ratio, i.e. on a long task, which is the worst possible moment.

use serde_json::{Value, json};
use shirley_agent_sdk::{
    Agent, ContentPart, ImagePart, ImageSource, Message, MessageContent, ModelConfig,
    ModelProtocol, Tool, ToolContext, ToolDefinition, ToolFuture, ToolManager,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn new(responses: Vec<String>) -> Self {
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
                    requests.push(request);
                    requests.len() - 1
                };
                let body = &responses[index.min(responses.len() - 1)];
                let header = format!(
                    "HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes());
                let _ = socket.write_all(body.as_bytes());
                let _ = socket.flush();
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn bodies(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
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

#[expect(
    clippy::needless_pass_by_value,
    reason = "test helper kept ergonomic at call sites; the JSON value is only read"
)]
fn completion(message: Value, prompt: u64, completion: u64) -> String {
    json!({
        "choices": [{ "message": message, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion }
    })
    .to_string()
}

struct Probe(ToolDefinition);

impl Tool for Probe {
    fn definition(&self) -> &ToolDefinition {
        &self.0
    }

    fn invoke(&self, _: Value, _: ToolContext) -> ToolFuture<'_> {
        Box::pin(async { Ok(json!("probe result")) })
    }
}

fn picture_turn(text: &str, file_id: &str) -> Message {
    Message::User {
        content: MessageContent::from(vec![
            ContentPart::Text { text: text.into() },
            ContentPart::Image(ImagePart {
                source: ImageSource::FileId {
                    file_id: file_id.into(),
                },
                detail: None,
            }),
        ]),
    }
}

fn filler(tag: usize) -> Message {
    Message::User {
        content: MessageContent::text(format!(
            "{tag} {}",
            "padding sentence for the cut ".repeat(80)
        )),
    }
}

#[tokio::test]
async fn compaction_and_main_requests_never_carry_a_file_id() {
    let server = Server::new(vec![
        // Round 1: the model asks for a tool, and reports a context that crosses the trigger
        // ratio, so compaction is scheduled for the next round.
        completion(
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": "probe", "arguments": "{}" }
                }]
            }),
            900,
            100,
        ),
        // Round 2 (a): the compaction request. Its input is the compressed region, which is where
        // the user's picture lives.
        completion(
            json!({ "role": "assistant", "content": "<current_goal>keep going</current_goal>" }),
            50,
            10,
        ),
        // Round 2 (b): the answer after compaction.
        completion(json!({ "role": "assistant", "content": "done" }), 5, 1),
    ]);

    let mut model = ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url(&server.url)
        .model("fixture")
        .stream(false)
        .build();
    model.context_window_tokens = Some(1_000);
    // The capability guard strips images before encoding when this is false, which would hide the
    // bug this test pins. A vision model is the only configuration where the encoder sees them.
    model.supports_images = true;

    let mut tools = ToolManager::new();
    tools
        .register(Probe(ToolDefinition {
            name: "probe".into(),
            description: "fixture".into(),
            parameters: json!({ "type": "object" }),
        }))
        .unwrap();

    let mut seed = vec![picture_turn("what is in this screenshot?", "pasted-1.png")];
    seed.extend((0..4).map(filler));

    let mut agent = Agent::builder()
        .model_config(model)
        .compaction_instruction("Preserve findings")
        .tools(tools)
        .messages(seed)
        .build()
        .unwrap();

    agent.run("continue").await.unwrap();

    let bodies = server.bodies();
    assert!(
        bodies.len() >= 3,
        "expected two rounds plus a summary request, got {}",
        bodies.len()
    );
    assert!(
        agent
            .messages()
            .iter()
            .any(|message| matches!(message, Message::ContextSummary { .. })),
        "compaction must have actually happened, otherwise this test proves nothing"
    );
    // The summary request is the one that used to blow up: its input is the compressed region,
    // which is exactly where the picture sits. Proving the text got there proves the reference
    // would have gone with it (otherwise this test proves nothing).
    let summary = bodies[1].to_string();
    assert!(
        summary.contains("what is in this screenshot?"),
        "the compacted region must actually be in the summary request: {summary}"
    );
    for (index, body) in bodies.iter().enumerate() {
        let rendered = body.to_string();
        assert!(
            !rendered.contains("file_id"),
            "request #{index} carried an unresolved image reference: {rendered}"
        );
    }
}
