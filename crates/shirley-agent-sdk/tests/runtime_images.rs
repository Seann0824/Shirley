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
    ModelProtocol, Tool, ToolCall, ToolContext, ToolDefinition, ToolFuture, ToolManager,
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

/// A tool reading a picture from the media library returns the envelope
/// `{ "content": [text, image(file_id)], "parts": [...] }`. The image part has to survive into the
/// `Tool` message, otherwise the resolver never runs and the model is handed a bare id instead of
/// the screenshot it asked for -- which is the bug this test pins.
struct Screenshot;

impl Tool for Screenshot {
    fn definition(&self) -> &ToolDefinition {
        // Leaked on purpose: the definition lives for the whole test process.
        static DEFINITION: std::sync::OnceLock<ToolDefinition> = std::sync::OnceLock::new();
        DEFINITION.get_or_init(|| ToolDefinition {
            name: "media".into(),
            description: "fixture".into(),
            parameters: json!({ "type": "object" }),
        })
    }

    fn invoke(&self, _: Value, _: ToolContext) -> ToolFuture<'_> {
        Box::pin(async {
            Ok(json!({
                "content": [
                    { "type": "text", "text": "shot.png (image/png)" },
                    {
                        "type": "image",
                        "source": { "kind": "file_id", "file_id": "pasted-1.png" }
                    }
                ],
                "parts": [{ "kind": "image", "source": { "kind": "artifact", "id": "pasted-1.png" } }]
            }))
        })
    }
}

#[tokio::test]
async fn a_tool_screenshot_reaches_the_model_as_an_image_not_an_id() {
    let server = Server::new(vec![
        // Round 1: the model asks to read the picture.
        completion(
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": "media", "arguments": "{}" }
                }]
            }),
            10,
            5,
        ),
        // Round 2: it answers, having (or not having) seen the picture.
        completion(json!({ "role": "assistant", "content": "done" }), 5, 1),
    ]);

    let mut model = ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url(&server.url)
        .model("fixture")
        .stream(false)
        .build();
    model.supports_images = true;

    let mut tools = ToolManager::new();
    tools.register(Screenshot).unwrap();

    let mut agent = Agent::builder()
        .model_config(model)
        .tools(tools)
        // The application layer wires this to the artifact store; here a fixed data URL stands in.
        .image_resolver(Arc::new(|id: &str| {
            (id == "pasted-1.png").then(|| "data:image/png;base64,iVBORw0KGgo=".to_owned())
        }))
        .build()
        .unwrap();

    agent.run("what is in the screenshot?").await.unwrap();

    let bodies = server.bodies();
    assert!(bodies.len() >= 2, "expected a tool round and an answer");
    let follow_up = bodies[1].to_string();
    assert!(
        follow_up.contains("data:image/png;base64,"),
        "the picture must reach the provider as a data URL: {follow_up}"
    );
    assert!(
        !follow_up.contains("file_id"),
        "an unresolved reference leaked to the provider: {follow_up}"
    );
    assert!(
        follow_up.contains("shot.png (image/png)"),
        "the text of the tool result must survive alongside the picture: {follow_up}"
    );
}

fn parallel_image_history(image_mask: u32) -> Vec<Message> {
    let calls = |ids: &[&str]| Message::Assistant {
        content: None,
        reasoning_content: None,
        tool_calls: ids
            .iter()
            .map(|id| ToolCall {
                id: (*id).into(),
                name: "probe".into(),
                arguments: "{}".into(),
            })
            .collect(),
    };
    let mut history = vec![
        Message::User {
            content: "inspect".into(),
        },
        calls(&["a", "b"]),
    ];
    // Results finish in reverse call order, just as FuturesUnordered may deliver them.
    for (bit, id) in ["b", "a"].into_iter().enumerate() {
        let mut parts = vec![ContentPart::Text {
            text: format!("result {id}"),
        }];
        if image_mask & (1 << bit) != 0 {
            parts.push(ContentPart::Image(ImagePart {
                source: ImageSource::FileId {
                    file_id: format!("{id}.png"),
                },
                detail: None,
            }));
        }
        history.push(Message::Tool {
            tool_call_id: id.into(),
            content: Some(MessageContent::parts(parts)),
        });
    }
    // Images must be flushed before a later assistant/tool group, not at request end.
    history.extend([
        calls(&["c"]),
        Message::Tool {
            tool_call_id: "c".into(),
            content: Some("result c".into()),
        },
    ]);
    history
}

/// Replaying the failing session must keep parallel results together even when the first
/// completed tool carries an image. Test both completion orders, multiple images, and text only.
#[tokio::test]
async fn parallel_tool_images_follow_the_complete_result_group_on_replay() {
    for protocol in [ModelProtocol::ChatCompletions, ModelProtocol::Responses] {
        for image_mask in 0u32..4 {
            let response = match protocol {
                ModelProtocol::ChatCompletions => {
                    completion(json!({ "role": "assistant", "content": "done" }), 5, 1)
                }
                ModelProtocol::Responses => json!({
                    "status": "completed",
                    "output": [{ "type": "message", "role": "assistant", "content": "done" }]
                })
                .to_string(),
                ModelProtocol::AnthropicMessages => unreachable!(),
            };
            let server = Server::new(vec![response]);
            let mut model = ModelConfig::builder()
                .protocol(protocol)
                .base_url(&server.url)
                .model("fixture")
                .stream(false)
                .build();
            model.supports_images = true;
            let mut agent = Agent::builder()
                .model_config(model)
                .messages(parallel_image_history(image_mask))
                .image_resolver(Arc::new(|id| Some(format!("data:image/png;base64,{id}"))))
                .build()
                .unwrap();
            agent.run("continue").await.unwrap();

            let bodies = server.bodies();
            assert_eq!(bodies.len(), 1);
            let (items, result_kind, result_id, first_result) = match protocol {
                ModelProtocol::ChatCompletions => {
                    (&bodies[0]["messages"], "tool", "tool_call_id", 2)
                }
                ModelProtocol::Responses => {
                    (&bodies[0]["input"], "function_call_output", "call_id", 3)
                }
                ModelProtocol::AnthropicMessages => unreachable!(),
            };
            let items = items.as_array().unwrap();
            for (offset, id) in ["b", "a"].iter().enumerate() {
                let item = &items[first_result + offset];
                assert_eq!(
                    item["role"].as_str().or(item["type"].as_str()),
                    Some(result_kind),
                    "{protocol:?} mask={image_mask}: image interrupted parallel results: {items:?}"
                );
                assert_eq!(item[result_id], *id);
            }
            let image_count = image_mask.count_ones() as usize;
            for offset in 0..image_count {
                let item = &items[first_result + 2 + offset];
                assert_eq!(item["role"], "user");
                assert!(
                    item["content"]
                        .to_string()
                        .contains("data:image/png;base64,")
                );
            }
            let next = &items[first_result + 2 + image_count];
            match protocol {
                ModelProtocol::ChatCompletions => assert_eq!(next["tool_calls"][0]["id"], "c"),
                ModelProtocol::Responses => assert_eq!(next["call_id"], "c"),
                ModelProtocol::AnthropicMessages => unreachable!(),
            }
            assert!(
                items.last().unwrap()["content"]
                    .to_string()
                    .contains("continue")
            );
            assert!(!bodies[0].to_string().contains("file_id"));
            // Only the request view moves images; the persisted tool result stays multimodal.
            assert_eq!(
                agent
                    .messages()
                    .iter()
                    .filter(|m| matches!(m,
                Message::Tool { content: Some(content), .. } if content.has_images()))
                    .count(),
                image_count
            );
        }
    }
}
