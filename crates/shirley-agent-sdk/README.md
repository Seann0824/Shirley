# shirley-agent-sdk

A business-agnostic Agent SDK for Rust. It packages the building blocks needed to run an LLM agent — message model, tool system, protocol adapters, a ReAct runtime, context compaction, a task ledger, process sandboxing, and workspace isolation.

The [Shirley](https://github.com/Seann0824/Shirley) TUI coding agent is built on top of it.

> **Status: `0.0.1`** — an early release; the API is still changing.

## Features

- **ReAct runtime** — `Agent` drives the full model → tool → model loop, with streaming support
- **Tool system** — a `#[tool]` attribute macro turns a plain async function into a model-callable tool (JSON Schema generated automatically)
- **Protocol adapters** — a unified `invoke` interface with three wire protocols: ChatCompletions, Responses, and Anthropic Messages (all with streaming)
- **Context compaction** — history is compacted automatically as the context window fills up; the summary is never shown to the user
- **Task ledger** — a `todo` tool lets the model maintain its own progress state, injected every turn so it survives compaction
- **Sandbox & workspace** — a unified process-execution abstraction (with timeouts and degradation reporting) and workspace path confinement
- **Unified error contract** — `ErrorKind` / `SdkError`; retry decisions are based on the kind, never on message text

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

### Tool-calling modes

Some models / endpoints do not support native tool calling at all. For those, the SDK
can drive tools through a **text protocol**: the tool catalog and an XML contract are
injected into the system prompt, the model writes tool calls as text, and the adapter
parses them back into `Message::Assistant.tool_calls` — so the ReAct loop and the
`AgentEvent` stream are **unchanged**.

This is a *transport mode*, orthogonal to `ModelProtocol`, and is selected with a single
field on `ModelConfig`:

```rust
use shirley_agent_sdk::ToolCallingMode;

// Default: use the provider's native tool_calls.
let config = ModelConfig::builder()
    .protocol(ModelProtocol::ChatCompletions)
    .base_url("https://api.example.com/v1/chat/completions")
    .model("some-model")
    .tool_calling(ToolCallingMode::Native)
    .build();

// Text protocol for models without native tool calling.
let config = ModelConfig::builder()
    .protocol(ModelProtocol::ChatCompletions)
    .base_url("http://localhost:11434/v1/chat/completions")
    .model("local-model")
    .tool_calling(ToolCallingMode::Tagged)
    .build();
```

Consumers do not need to know which mode is active: tool calls surface as the same
`AgentEvent::ToolStarted` / `ToolFinished`, and streaming text as the same
`ContentDelta` / `ReasoningDelta` (the tool block itself is filtered out of the stream).
Everything specific to the text protocol — the prompt, the request encoding, the parser —
lives inside `src/adapter/`; `runtime/` has no protocol knowledge.

> **Auto-selection is a host concern.** The SDK only exposes the field; choosing a mode
> per model (e.g. a `tool_mode` column in a model catalog) is up to the embedding
> application.


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

## Examples

Runnable examples live in [`examples/`](examples/). Model-backed ones read the endpoint from
the environment (`LOCAL_BASE_URL`, `LOCAL_API_KEY`, `LOCAL_MODEL`); the rest run locally with
no network.

| Example | What it shows |
| --- | --- |
| `quickstart` | The smallest end-to-end run: one tool, one turn (`Agent::run`) |
| `streaming` | Consuming the `AgentEvent` stream: deltas, tool activity, usage |
| `tools` | A stateless tool, a stateful tool reading `ToolContext`, and `on_register` / `on_unregister` hooks |
| `context_provider` | Appending application-owned context to every request via `ContextProvider` |
| `sandbox` | Running a command with a timeout and inspecting `SandboxOutput` / degradation |
| `workspace` | `WorkSpace::resolve` path confinement (`OutsideRoot` vs `InvalidPath`) |
| `token_counting` | The CJK-aware token heuristic used for compaction cut points |
| `protocols` | Building the three protocol configs and the `ToolCallingMode` switch |
| `real_tool_calling` | Native vs Tagged tool calling against a real endpoint |

```sh
cargo run -p shirley-agent-sdk --example quickstart
cargo run -p shirley-agent-sdk --example sandbox
```

## Core types

| Type | Description |
| --- | --- |
| `Agent` | The ReAct runtime; built with a `bon` builder, driven by `run` / `run_stream` |
| `ModelConfig` | Model endpoint config: protocol, base_url, model, api_key, stream, thinking, etc. |
| `ModelProtocol` | Protocol enum: `ChatCompletions` / `Responses` / `AnthropicMessages` (all implemented) |
| `ToolCallingMode` | Tool transport: `Native` (provider `tool_calls`) or `Tagged` (text/XML protocol for models without native tool calling) |
| `Message` | Message enum: System / User / Assistant / Tool / ContextSummary |
| `ToolManager` | Tool registration and invocation; `definitions()` is sorted by name to keep the prompt prefix stable |
| `Tool` / `ToolError` | The tool trait and its error type (tool authors only return `ToolError`) |
| `Usage` | Token accounting; distinguishes "reported zero" from "not reported" (`Option<u64>`) |
| `SystemPrompt` | The system prompt: a fixed string, or a function that generates one from runtime context |
| `sandbox` | `Sandbox` / `SandboxSpec` / `ProcessBackend` — process execution and isolation abstraction |
| `workspace` | `WorkSpace::resolve` — confines paths to the workspace root |

## Requirements

- Rust edition 2024
- An async runtime (the examples use `tokio`)

## License

Licensed under either of MIT or Apache-2.0, at your option.
