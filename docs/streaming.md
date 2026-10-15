**Shirley technical design · Streaming output (P1)**

**1. Current state (landed; this document has become a "design description of an implemented capability")**

Streaming is already wired up: the `"stream"` field in `encode_request` reads `ModelConfig.stream` (configured as `true` in `main.rs`), the response side does SSE incremental parsing in `chat_completions::decode_stream_response`, and `AgentEvent` yields `ContentDelta` / `ReasoningDelta` chunk by chunk, which the TUI's `App::append_streaming_delta` appends.

```rust
// adapter/chat_completions/mod.rs (current)
let body = serde_json::json!({
    "model": &config.model,
    "messages": encode_messages(&input.messages),
    "tools": encode_tools(&input.tools),
    "thinking": { "type": thinking },          // reads config.thinking
    "reasoning_effort": &config.reasoning_effort,
    "stream": &config.stream,                  // reads config.stream
});
```

The event names are `ContentDelta` (not the `TextDetal` from the early draft), and reasoning uses `ReasoningDelta`. `invoke` in `adapter/mod.rs` branches on `config.stream`; `runtime` converts `AdapterEvent::ContentDelta` / `ReasoningDelta` directly into `AgentEvent`.

**Gaps that still exist**: the SSE `id:` line in `decode_stream_response` is still a `todo!()`; `Agent::run_stream` has no test covering streaming/non-streaming consistency.

**2. Why it is not just an experience issue**

In non-streaming mode, during a long answer (or long reasoning) the TUI can only show "AI replying Ns". The user cannot tell whether it is thinking, generating, or stuck. For a Coding Agent doing tasks that routinely take tens of seconds, this directly affects "do I dare hand it over". At the same time, streaming is also the precondition for the later "execute while generating".

**3. Design points**

**3.1 Protocol layer: SSE decoding (implemented)**

ChatCompletions streaming is `text/event-stream`, with each line `data: {...}`, ending with `data: [DONE]`. The current implementation does not have a separate `SseDecoder` struct; instead, inside `decode_stream_response` it uses a `buffer: String` and splits events by `\n\n`:

```rust
// crates/shirley-agent-sdk/src/adapter/chat_completions/mod.rs (current)
let mut buffer = String::new();
while let Some(chunk) = byte_stream.next().await {
    buffer.push_str(&String::from_utf8_lossy(&chunk));
    while let Some(pos) = buffer.find("\n\n") {
        // take one complete event, parse line by line: event:/data:/id:
    }
}
```

Points to note: chunk boundaries can split JSON, so parse by event boundary (blank line), not by chunk boundary; `[DONE]` is treated as a terminating sentinel and skipped, without JSON parsing. (The `id:` line still leaves a `todo!()`.)

**3.2 Incremental assembly**

Each streaming delta carries only a fragment, and they must be accumulated into a complete message:

```rust
#[derive(Default)]
struct StreamAccumulator {
    content: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, ToolCallAccumulator>, // aggregated by index
    finish_reason: Option<String>,
    usage: Option<Usage>,
}
```

The delta for tool calls deserves special attention: `function.arguments` is a **JSON string delivered in fragments**, and it must be aggregated by `index` before parsing; do not try `from_str` on every fragment.

**3.3 Event production (implemented; naming differs slightly from the draft)**

What `AgentEvent` actually adds is:

```rust
pub enum AgentEvent {
    ContentDelta(String),      // text increment (called TextDelta in the draft)
    ReasoningDelta(String),    // thinking increment
    // ... existing events
}
```

Note there is no separate `ToolCallDelta`: tool-call increments are aggregated inside `decode_stream_response` (`merge_tool_call_deltas` + `finish_tool_calls`), and the complete `tool_calls` is delivered only at the end of the stream via `Finished(ModelResponse)`.

**3.4 Consistency after the stream ends**

Key constraint: **the `Message` ultimately persisted must be exactly the same for streaming and non-streaming**. The current implementation accumulates `content` / `reasoning_content` / `tool_calls` inside `decode_stream_response`, and on receiving `finish_reason` it produces an `AdapterEvent::Finished(ModelResponse)`, after which it goes through exactly the same persistence logic as now. This way compaction, `active_messages`, and `RunResult` do not need to distinguish the two modes.

(The standalone `StreamAccumulator` struct proposed in the draft was not extracted separately; the accumulation logic is inlined directly in the streaming decode function.)

**4. Rework of `run_stream` (implemented)**

The `Agent` method is already named `run_stream` (referring to the event stream); whether the model streams is decided by `ModelConfig.stream`. `adapter::invoke` branches on `config.stream` (`adapter/mod.rs`): streaming consumes the event stream from `decode_stream_response` and `yield`s, non-streaming parses the full response and then `yield`s a single `Finished`.

```rust
// adapter/mod.rs (current)
if !config.stream {
    // full response -> yield AdapterEvent::Finished(msg)
} else {
    // decode_stream_response -> yield event by event
}
```

(The `GenerationConfig` / `ModelOutput` enums mentioned in the draft were not introduced; the switch sits directly on `ModelConfig`.)

**5. TUI side (implemented)**

The hand-written translation layer `AgentUpdate` has been removed, and `tui::apply` consumes `AgentEvent` directly:

- On `ContentDelta` -> `App::append_streaming_delta(delta, false)`, appended to "the Assistant message currently being generated" (`streaming_delta_start` points at the start; it does not add a new one).
- On `ReasoningDelta` -> `App::append_streaming_delta(delta, true)`, appended to "the current thinking block".
- On `MessageAdded(Assistant)` -> `App::finish_streaming_deltas()` ends the streaming block and turns it into the final message.

`App` already has `streaming_delta_start: Option<usize>` to express "the message in progress".

**6. Performance and caching notes**

- Streaming does **not affect** the prefix cache: the request-body prefix (system + history messages + tool definitions) is unchanged; `stream: true` is only a response-side difference. `thinking` / `reasoning_effort` now read `ModelConfig` (no longer hard-coded); changing these configs changes the request prefix and breaks the cache -- keep this in mind.
- The sorting of `ToolManager::definitions()` (by name) must be kept; `Agent.md` specifically warned about this.
- Rebuilding `MessageCache` every frame becomes a high-frequency operation under streaming, and it must be changed to incremental appending (see the performance section of `testing.md`).

**7. Acceptance**

1. With `stream: true` enabled, the TUI shows the answer character by character, and reasoning is visible (controlled by `Ctrl+T`).
2. For the same prompt, streaming and non-streaming persist the same `Message::Assistant` content. (No test guards this yet.)
3. Tool calls can correctly aggregate a complete `arguments` JSON under streaming. (`merge_tool_call_deltas` is implemented.)
4. On a mid-stream cancel, the already-generated part is preserved and no half-corrupted message is produced. (The cancellation channel is not implemented yet; see section 4 of `runtime-hardening.md`.)
