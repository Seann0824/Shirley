# shirley-agent-sdk

A business-agnostic Agent SDK for Rust. It packages the building blocks needed to run an LLM agent — message model, tool system, protocol adapters, a ReAct runtime, context compaction, recall, process sandboxing, and workspace isolation.

The [Shirley](https://github.com/Seann0824/Shirley) TUI coding agent is built on top of it.

> **Status: `0.0.1`** — an early release; the API is still changing.

## Features

- **ReAct runtime** — `Agent` drives the full model → tool → model loop, with streaming support
- **Tool system** — a `#[tool]` attribute macro turns a plain async function into a model-callable tool (JSON Schema generated automatically)
- **Protocol adapters** — a unified `invoke` interface with three wire protocols: ChatCompletions, Responses, and Anthropic Messages (all with streaming)
- **Context compaction** — history is compacted automatically as the context window fills up; the summary is never shown to the user
- **Recall** — compacted conversational content goes into a BM25 index the model can query on demand
- **Sandbox & workspace** — a unified process-execution abstraction (with timeouts and degradation reporting) and workspace path confinement
- **Unified error contract** — `ErrorKind` / `SdkError`; retry decisions are based on the kind, never on message text
- **Session persistence** — a `SessionStore` trait supporting restore and rewind

## Installation

```sh
cargo add shirley-agent-sdk
```

`shirley-agent-sdk-macros` (the implementation of the `#[tool]` macro) is pulled in automatically as a transitive dependency — no need to add it yourself.

## Quick start

```rust
use shirley_agent_sdk::{Agent, ModelConfig, ModelProtocol, ToolError, ToolManager, tool};

// Define a tool with the #[tool] macro. Parameter descriptions are required.
#[tool(description = "Add two numbers")]
async fn add(
    #[param(description = "Left operand")] left: i64,
    #[param(description = "Right operand")] right: i64,
) -> Result<i64, ToolError> {
    Ok(left + right)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model_config = ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        // base_url is the full request endpoint, not a base path.
        .base_url("https://api.example.com/v1/chat/completions")
        .api_key("sk-...")
        .model("gpt-4o-mini")
        .stream(true)
        .build();

    let mut tools = ToolManager::new();
    tools.register(add::tool())?;

    let mut agent = Agent::builder()
        .model_config(model_config)
        .tools(tools)
        .build()?;

    // run() blocks until the turn finishes and returns a RunResult.
    let result = agent.run("What is 2 + 3?").await?;
    println!("{:?}", result.messages);
    Ok(())
}
```

### Choosing a protocol

`base_url` is the full request endpoint — the adapter POSTs to it as-is.

```rust
// OpenAI Chat Completions (default)
let config = ModelConfig::builder()
    .protocol(ModelProtocol::ChatCompletions)
    .base_url("https://api.deepseek.com/v1/chat/completions")
    .api_key("sk-...")
    .model("deepseek-chat")
    .build();

// OpenAI Responses
let config = ModelConfig::builder()
    .protocol(ModelProtocol::Responses)
    .base_url("https://api.deepseek.com/responses")
    .api_key("sk-...")
    .model("deepseek-chat")
    .build();

// Anthropic Messages — the adapter adds `x-api-key` and `anthropic-version` for you.
let config = ModelConfig::builder()
    .protocol(ModelProtocol::AnthropicMessages)
    .base_url("https://api.deepseek.com/anthropic/v1/messages")
    .api_key("sk-...")
    .model("deepseek-chat")
    .build();
```

Everything downstream of `ModelConfig` is protocol-agnostic: the same `Agent`, `Message`, and tool code runs against any of the three. Protocol quirks (system handling, tool-result placement, thinking blocks, usage semantics, streaming event shapes) live entirely in `src/adapter/`.

### Consuming the stream

`run_stream()` returns a stream of events, suitable for driving a UI:

```rust
use futures::StreamExt;
use shirley_agent_sdk::AgentEvent;

let mut stream = agent.run_stream("Hello");
while let Some(event) = stream.next().await {
    match event? {
        AgentEvent::ContentDelta(text) => print!("{text}"),
        AgentEvent::ReasoningDelta(text) => eprint!("{text}"),
        AgentEvent::ToolStarted { name, .. } => println!("[tool call] {name}"),
        AgentEvent::Finished(result) => println!("\ndone, cache hit rate {:?}", result.cache_hit_rate()),
        _ => {}
    }
}
```

## Core types

| Type | Description |
| --- | --- |
| `Agent` | The ReAct runtime; built with a `bon` builder, driven by `run` / `run_stream` |
| `ModelConfig` | Model endpoint config: protocol, base_url, model, api_key, stream, thinking, etc. |
| `ModelProtocol` | Protocol enum: `ChatCompletions` / `Responses` / `AnthropicMessages` (all implemented) |
| `Message` | Message enum: System / User / Assistant / Tool / ContextSummary |
| `ToolManager` | Tool registration and invocation; `definitions()` is sorted by name to keep the prompt prefix stable |
| `Tool` / `ToolError` | The tool trait and its error type (tool authors only return `ToolError`) |
| `Usage` | Token accounting; distinguishes "reported zero" from "not reported" (`Option<u64>`) |
| `SystemPrompt` | The system prompt: a fixed string, or a function that generates one from runtime context |
| `SessionStore` | Session persistence trait (`append` / `load` / `truncate`) |
| `sandbox` | `Sandbox` / `SandboxSpec` / `ProcessBackend` — process execution and isolation abstraction |
| `workspace` | `WorkSpace::resolve` — confines paths to the workspace root |

## Requirements

- Rust edition 2024
- An async runtime (the examples use `tokio`)

## License

Licensed under either of MIT or Apache-2.0, at your option.
