**Shirley -- Project Agent Guide**

This document is written for the next Agent (or human) who takes over this repository. I have tried to make it read like a manual you can work directly from, not a piece of marketing. If all you want to know is "where do I start changing code", jump straight to the **Getting Started** section.

> This document describes the **current true state of the code**. If you change the architecture, please bring this in line too -- a previous version misled people because it lagged behind by a dozen commits.

---

**1. What this is**

Shirley is a Coding Agent written in Rust. The name comes from Shirley in *Code Geass*; the character setup in `prompts/system.md` (the system prompt) is written around her -- it is both a personality test case and the real default persona.

The whole repository is split into three layers:

| Layer | Location | Responsibility |
| --- | --- | --- |
| Application layer | `src/` | A runnable TUI chat Agent that registers tools and drives the interface |
| SDK layer | `crates/shirley-agent-sdk/` | Business-agnostic Agent fundamentals: messages, tools, protocol adapters, the ReAct runtime, sandbox, workspace |
| Macro layer | `crates/shirley-agent-sdk-macros/` | The `#[tool]` attribute macro that turns an ordinary async function into a model-callable tool |

The core design goal is written in the root `plan.md`: the SDK should only accumulate "capabilities you would still need the next time you build an Agent", with no business logic mixed in. The current focus is the "Coding Agent" scenario, i.e. using it for long tasks such as migrating a TS backend to Rust.

**Prompts always go through md (a convention)**: the **body text** of every system prompt / injected copy / compression template is maintained in `prompts/*.md` (application layer) and `crates/shirley-agent-sdk/prompts/*.md` (SDK). Rust code only `include_str!`s the template and fills dynamic values via `{{placeholder}}` (`src/prompt_template.rs::render`, a single pass, unknown placeholders kept verbatim). Changing copy does not require changing code; to add dynamic content, add a placeholder. Current templates: `system.md` / `project_guide.md` / `project_guide_missing.md` / `compression_instruction.md` / `todo_header.md` / `todo_nag.md` / `skills_header.md` / `curator_*.md` (application layer), `compaction_template.md` (SDK).

**`docs/` is a set of independent technical design documents** (not the same thing as the root `plan.md`):

| Document | What it solves |
| --- | --- |
| `docs/README.md` | Document index + three global design principles |
| `docs/architecture.md` | Architecture diagram + where each feature lives |
| `docs/roadmap.md` | Overall roadmap: phases, dependencies, acceptance criteria |
| `docs/security.md` | Permission layer, bash boundary, secrets, workspace isolation |
| `docs/sandbox.md` | Process sandbox: spec / backend / degraded / timeout |
| `docs/runtime-hardening.md` | max_steps, retry, timeout, cancellation, compaction robustness |
| `docs/streaming.md` | Streaming output and reasoning streaming |
| `docs/adapter-layer.md` | Protocol adapter middle layer, tool-argument normalization, multi-protocol |
| `docs/responses-api.md` | Responses protocol adapter: request item expansion / response decoding / streaming events (**implemented**) |
| `docs/anthropic-messages-api.md` | Anthropic Messages protocol adapter: content blocks / streaming fragment aggregation / usage semantics (**implemented**) |
| `docs/todo.md` | The `todo` task ledger: model self-maintained, task state that survives compaction / injected at the end of each round (**implemented**; the ledger lives in the application layer `src/todo.rs`, the SDK only keeps the generic seam `ContextProvider`) |
| `docs/testing.md` | SDK unit-test strategy, cache hit-rate benchmarks |
| `docs/plan.md` | Error-handling unification: completed state + follow-up task list |
| `docs/tool-lifecycle.md` | Tool lifecycle: `ToolContext` belongs to `ToolManager` / `on_register` (created) / `on_unregister` (destroy) / `unregister` (**implemented**) |
| `docs/tool-macro.md` | Tool macro expressiveness: function macro extended with optional companion hooks, lifecycle wired by the macro (**implemented**) |
| `docs/desktop-interface.md` | Desktop interface: Tauri 2 + React migration of the shiwen chat UI / shared LCA (`AgentEvent`) / stripping shiwen custom logic (**M0-M3 landed**, including `@` reference rebinding to workspace files, markdown rendering alignment, fixed chat width `max-w-190`, consecutive tool calls collected into a collapsible area, **multi-project multi-workspace**: register / switch / rename / archive / restore) |
| `docs/skills.md` | Skills: embracing the Agent Skills open standard (`SKILL.md` directory / frontmatter) / progressive disclosure (catalog always resident + activate on demand + read resources on demand) / `SkillContextProvider` appended injection at the end of each round (catalog + activated body, survives compaction, KV-friendly) / application-layer implementation, zero SDK change (**P0+P1+P2(slash) landed**: `src/skills/` discovery + parsing + catalog + `activate_skill` / `SkillRuntime` activation + `read_skill_file` resource reading + `/<skill-name>` command-layer direct activation) |
| `docs/multimodal.md` | Multimodal input (images): SDK mechanism (structured `ContentPart` in `Message` / image encoding across ChatCompletions + Responses + Anthropic / `ModelConfig.supports_images` + send-time guard) vs application-layer policy (capability source, attach-image UI, session stores references not bytes) (**proposal, not landed**) |

---

**2. Overall architecture**

The data flow is roughly this:

```
User input
  -> Agent::run_stream(task)
      -> assemble active_messages (compress context first if necessary)
      -> adapter::invoke returns an AdapterEvent stream
          -> streaming: ReasoningDelta / ContentDelta increments
          -> finish: Finished(ModelResponse)
      -> persist the Assistant message
      -> if there are tool_calls, run the tools concurrently (sandbox)
      -> persist Tool messages
      -> back to the model, until there are no more tool calls
  -> produce an AgentEvent stream (consumed by the UI)
  -> give a RunResult at the end
```

**The SDK exposes only these** (see `crates/shirley-agent-sdk/src/lib.rs`):

- `Agent`, `AgentError`, `AgentEvent`
- `SystemPrompt`, `SystemPromptContext` (system prompt: a static string or a function)
- `CutPlan`, `CompactParts`, `plan_cut` (compaction cut point / rebuild, for tests and upper-layer observation)
- `Message`, `ToolCall`, `Usage`
- `ModelConfig`, `ModelProtocol`, `AdapterError`
- `ErrorKind`, `SdkError` (unified error contract)
- `tool` (macro), `ToolManager`, `Tool`, `ToolDefinition`, `ToolError`, `ToolContext`
- `sandbox` (`Sandbox` / `SandboxSpec` / `SandboxOutput` / `SandboxBackend` / `ProcessBackend` / `SandboxError` / `NetworkPolicy` / `Capabilities`)
- `workspace` (`WorkSpace` / `WorkspaceError`)
- `ContextProvider` -- the generic seam for **appending context at the end of each request** (the application layer uses it to inject the task ledger etc.; the SDK does not build in any concrete content)
- `token` (`TokenCounter` / `HeuristicCounter` / `count_text` / `count_message` / `count_messages`) -- token accounting

The only truly private modules are `message` / `adapter` / `runtime` (and the internal implementation of `tool`) -- they are not directly `pub mod`, and only re-export the necessary types through the `pub use` above. When changing these modules, be careful not to break the external contract.

---

**3. Key module quick reference**

**1. Message model** -- `crates/shirley-agent-sdk/src/message/mod.rs`

`Message` is an enum with `#[serde(tag = "role")]`, with five variants:

- `System` / `User`: plain text
- `Assistant`: optional `content`, optional `reasoning_content`, a `tool_calls` list
- `Tool`: `tool_call_id` + optional content
- `ContextSummary`: **not shown to the user**, degraded to a system message when encoding a request, the product of context compaction

`Usage` is important: it deliberately distinguishes "reported 0" from "not reported":

- `cached_input_tokens: Option<u64>` -- `None` means the provider did not report it
- `cache_reported_input_tokens: Option<u64>` -- the true denominator (coverage) of the cumulative hit rate
- `cache_hit_rate()` returns `None` when not reported; **do not treat "cannot measure" as a 0% hit rate**
- The `Add` impl does "keep the known side" for `Option`, not add-as-0

**2. Unified error contract** -- `crates/shirley-agent-sdk/src/error.rs`

- `ErrorKind`: stable error classification (`Transport` / `RateLimited` / `ServerError` / `BadRequest` / `Unsupported` / `ToolFailure` / `Internal`), used for logs, metrics, retry decisions
- `SdkError` trait: `kind()` + `is_retryable()` (derived from `ErrorKind::is_retryable` by default)
- Each layer's error (`AdapterError` / `ToolError` / `SandboxError` / `WorkspaceError` / `AgentError`) **stays in its own module**, but all implement `SdkError`. `AgentError` converges them via `#[from]`
- The display format is unified as `[prefix]: detail`, and the prefix stays next to the variant, not derived from `ErrorKind`
- **Decisions look at classification, not wording**: to decide whether to retry, rely only on `SdkError::is_retryable`, never `match` on the error string

**3. Protocol adapter layer** -- `crates/shirley-agent-sdk/src/adapter/`

- `ModelConfig` uses `bon` to generate a builder: `protocol` / `base_url` / `model` / `api_key` / `stream` / `thinking` / `reasoning_effort` / `temperature` / `max_output_tokens` / `context_window_tokens` / `tool_choice` / `extra_body`
- `codec(protocol)` returns a trio of function pointers `(Encoder, Decoder, StreamDecoder)` (request encoding / non-streaming decoding / streaming decoding; streaming is also brought into the codec abstraction, and `invoke` no longer has hard protocol branches). **All three protocols are implemented** (`ChatCompletions` / `Responses` / `AnthropicMessages`); the `UnsupportedProtocol` variant is kept for future protocols
- `invoke()` returns `Stream<Item = Result<AdapterEvent, AdapterError>>`:
  - non-streaming: read the full body, decode, produce a single `Finished(ModelResponse)`
  - streaming: go through `decode_stream_response`, yielding `ReasoningDelta` / `ContentDelta` / `Finished` chunk by chunk
- `ensure_success` is pulled out separately for status checking, ensuring the error body and status code are kept together (the sole basis for retry decisions)
- In `chat_completions/mod.rs`, `encode_messages` / `encode_tools` do the "internal Message -> OpenAI format" conversion; `dto.rs` only defines the deserialization structures
- `temperature` / `max_output_tokens` / `tool_choice` are wired up (written into the request body only when `Some`; on ChatCompletions the key for `max_output_tokens` is `max_tokens`); `extra_body` is the escape hatch for overriding the request body (shallow merge, `null` deletes a key); `thinking` writes `{"type":"enabled"}` only when enabled. See `docs/sdk-gaps.md` gap-4
- Responses decoding aligns with the official OpenAI schema (DeepSeek is essentially an OpenAI-compatible implementation): reasoning takes `summary` (`summary_text`, the OpenAI family returns only this by default) first, falling back to `content` (`reasoning_text`, which DeepSeek uses); streaming recognizes both `response.reasoning_text.delta` and `response.reasoning_summary_text.delta`; **non-streaming `status:"failed"` turns into `AdapterError` just like streaming** (`cancelled`/`queued` go to `Other`, not a false `Stop`); historical assistant text uses a **plain string** content (`output_text` belongs only to output items, and the input side does not accept it)
- Responses' `encode_input` is a **pure order-preserving mapping**: system stays as an ordinary `{role:"system"}` item in `input` in its original position, **not using the top-level `instructions`** (the latter is inserted by the server before `input`, moving a mid/late system to the very front and breaking the KV prefix cache). Order preservation = appending a message does not touch the prefix, which is the structural guarantee of cache hits; **do not add sort / grouping / position adjustment here**

Known design debt: `encode_messages` is written in the adapter layer, but the author's own comment says "this logic should really be on the message side". In `encode_request`, `thinking` is now mapped to `enabled` / `disabled` according to `config.thinking` (**streaming is implemented**, no longer hard-coded).

**4. Tool system** -- `crates/shirley-agent-sdk/src/tool/mod.rs`

- `Tool` trait: `definition()` gets the metadata, `invoke(Value, ToolContext)` returns `ToolFuture`; plus **two default hooks** `on_register` (= created) / `on_unregister` (= destroy), defaulting to `Ok(())` -- stateless tools need zero changes. `executed` is just `invoke` itself; there is no separate hook. See `docs/tool-lifecycle.md`
- `ToolManager`: holds the `ToolContext` (**no longer a separate builder parameter of `Agent`**); checks for duplicates on registration, and `definitions()` **sorts by name** (this is so the prefix is stable and the cache hit rate improves -- do not casually delete this sort). Adds `unregister(name)` (remove from the table first -> then call `on_unregister`, best-effort, returns `NotFoundError` if not registered), `context()` / `context_mut()`
- `ToolContext`: a type-erased runtime context container (`Arc<HashMap<TypeId, Arc<dyn Any>>>`). Tools run concurrently, so the context can only be an interior-mutability handle such as `Arc<Mutex<T>>`; a tool retrieves it with `ctx.get::<T>()`. Two ways to write: **the registration hook `ctx.insert(value)`** (recommended, registration and injection are one step, so they cannot drift) or the application layer's `with(data)`; to clean up use `ctx.remove::<T>()` (by `TypeId`, **each tool should use its own newtype state** to avoid accidentally removing the same type). The runtime does `self.tools.context().clone()` before each call to dispatch (`Arc`, only bumps the refcount). See `docs/sdk-gaps.md` gap-1, `docs/tool-lifecycle.md`
- `ToolError` has four categories: `ExecutionError` / `RepetitionError` / `NotFoundError` / `ArgumentsError`

About `ToolDefinition.parameters`: it is directly JSON Schema. It was originally recorded as debt "hard-wired to OpenAI", but this was corrected while doing the Responses adapter -- **Responses' `tools[].parameters` is also JSON Schema**, just without the extra `function` wrapper. So JSON Schema itself is the cross-protocol intermediate representation, and there is no need to invent a `ParamType`; the real protocol differences are in the **message item structure** and the **streaming event semantics** (see `docs/responses-api.md`, `docs/adapter-layer.md`).

**5. The `#[tool]` macro** -- `crates/shirley-agent-sdk-macros/src/tool.rs`

Usage:

```rust
#[tool(description = "bash is used to run commands")]
pub async fn bash(
    #[param(description = "the command to run")] command: String,
    #[param(description = "timeout (seconds)")] timeout: Option<u64>,
) -> Result<String, shirley_agent_sdk::ToolError> { ... }
```

A stateful tool may declare one `ToolContext` (or `&ToolContext`) parameter -- it **does not go into the argument schema**, is injected by the SDK at call time, and is passed to the function in its original position (at most one per tool):

```rust
#[tool(description = "increment the counter")]
async fn bump(ctx: &ToolContext, #[param(description = "increment")] by: u32) -> Result<u32, ToolError> {
    let session = ctx.get::<Arc<Mutex<Session>>>()
        .ok_or_else(|| ToolError::ExecutionError("session not provided".into()))?;
    let mut guard = session.lock().unwrap();
    guard.counter += by;
    Ok(guard.counter)
}
```

Optional **companion hooks**: `on_register` / `on_unregister` take ordinary user-written functions (with a fixed signature `fn(&mut ToolContext) -> Result<(), ToolError>`), and the macro wires them into the generated `impl Tool` -- the user only writes the implementation and never touches the `Tool` trait / schema (this is a hard constraint: "the macro is the only entry point"). No hook written = empty impl:

```rust
#[tool(description = "web search", on_register = ws_on_register, on_unregister = ws_on_unregister)]
async fn web_search(ctx: &ToolContext, #[param(description = "query")] query: String)
    -> Result<String, ToolError> { ... }

fn ws_on_register(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.insert(WebSearchState::from_env()?);   // inject the dependency at registration time
    Ok(())
}
fn ws_on_unregister(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.remove::<WebSearchState>();
    Ok(())
}
```

The hook definitions still live on the `Tool` trait; the macro only "references" them; see `docs/tool-lifecycle.md`, `docs/tool-macro.md`.

After expansion, the macro generates, in a `mod` of the same name: an `Arguments` struct (`Deserialize` + `JsonSchema` + `deny_unknown_fields`), `GenerateTool` (implements `Tool`, including hook overrides), `definition()`, `tool()`. The caller just writes `tools::bash_tool::tool()` to register it.

Macro constraints (pitfalls we hit):

- The function cannot use `self`, and parameters cannot be destructured or use `ref` / `@` bindings
- The parameter description must be written as `#[param(description = "...")]`, otherwise it is a compile error
- The generated `invoke` calls `super::#name`, so **the annotated function and the macro-generated mod must be at the same level** (same for hooks: `on_register = f` requires `f` to be at the same level as the tool function, and the macro generates a `super::f` reference)

**6. The ReAct runtime** -- `crates/shirley-agent-sdk/src/runtime/`

This is the heart of the whole project. It is split by responsibility into several submodules, and `mod.rs` only does the wiring and re-exports:

- `agent.rs`: the `Agent` itself (construction, the `run` / `run_stream` main loop, compaction scheduling)
- `compaction.rs`: compaction cut point and rebuild (`CompressionConfig` / `CutPlan` / `CompactParts` / `plan_cut` / `background_len`)
- `context.rs`: the append-context seam at the end of each request (`ContextProvider`)
- `event.rs`: external events and run results (`AgentEvent` / `RunResult` / `StopReason`)
- `error.rs`: top-level error convergence (`AgentError` + `SdkError`)
- `prompt.rs`: system prompt (`SystemPrompt` / `SystemPromptContext`)

The `CompactParts` contract test moved to `crates/shirley-agent-sdk/tests/runtime_compaction.rs` (an integration test that depends only on the public API).

- `Agent::new` is a `bon` builder: `model_config` / `system_prompt` / `working_dir` / `messages` / `tools` / `compression_instruction` / `compression_config` / `context_provider`. **The SDK does not hold a session**: there is no `session` field; persistence / restore is entirely the application layer's job (see `docs/session.md`). **The SDK also does not build in a task ledger** (it moved to the application layer) -- it only provides the `context_provider` seam
- `system_prompt`'s type is `SystemPrompt` (not `String`): it accepts both a fixed string (`From<String>` / `From<&str>`) and a **function** `Fn(&SystemPromptContext) -> String`. The function form lets the prompt be generated dynamically from the runtime context -- `SystemPromptContext` currently carries `working_dir`. `working_dir` is a separate builder parameter, and the prompt is resolved once at construction time using it
- `run()` is a thin wrapper around `run_stream()` that just waits for the last `Finished`
- Model calls have a 180-second progress deadline (configurable through `ModelConfig.request_timeout`), bounded retries before output, and at most two continuations after a partial transport failure. Completed tool results are retained; incomplete tool fragments are discarded. Desktop turn supervision restores the Agent and emits a terminal event even after a panic; title generation cannot delay task completion. See `docs/runtime-recovery.md`.
- Hot-swap seams for run parameters (used by application-layer commands, none of which rebuild the `Agent`): `set_model(model)` only changes the model name; `set_provider(base_url, api_key)` changes the endpoint and key (symmetric with the former, used by `/login`); `unregister_tool(name)` unregisters a tool at runtime (triggering its `on_unregister`, symmetric with `ToolManager::register`). These are seams the SDK added for application-layer commands, and the remaining fields of the model config are kept as-is. **`switch_session` has been removed from the SDK** (multi-session refactor, `docs/multi-session.md` decision 9), and **the `SessionStore` contract and `session` field have also moved out of the SDK** (`docs/session.md`) -- "which session is active" is application-layer orchestration; the SDK no longer has a concept of a "current session", nor does it hold a session log; each `Agent` corresponds to a fixed working set, switching is done by the application layer's `SessionManager.active` pointer, and persistence is event-driven by the application layer
- **The ownership boundary of `messages`**: `self.messages` is the **SDK's private working set**; the business log (`SessionStore`) is the source of truth, and the two are bridged by the one-way `MessageAdded` event. The business side has only two entries into SDK state: **seed** (via `builder.messages(...)` at construction) and **`truncate_messages(len)`** (tail truncation). **The SDK does not provide `add` / `push` / `insert`** -- writing user / assistant / tool into `self.messages` is a protocol side effect of the `run_stream` main loop, not a public API; and you should **not** add a "pre-add mutation hook": read-only observation is already covered by `MessageAdded`, whereas a rewritable / vetoable hook would introduce a second writer, smash the prefix cache, and externalize the loop invariants -- exactly the muddiness we want to eliminate. If you need "redact / rewrite / inject before sending", use the **outbound transform seam** (same family as `ContextProvider`: it only affects this round's request view and **does not touch `self.messages`**), do not touch the stored history. In one sentence: what should be handed over is the "right to record", not the "right to mutate core state"
- `run_stream()` returns `Pin<Box<dyn Stream<Item = Result<AgentEvent, AgentError>> + Send + 'a>>`, implemented with `async_stream::try_stream!`
- The main loop: compaction check -> assemble the request -> call the model (consume `AdapterEvent`) -> persist -> if there are tool_calls run them concurrently (`FuturesUnordered`) -> if there are no tool calls, `Finished` and exit
- `AgentEvent` has: `ContentDelta` / `ReasoningDelta` (streaming increments), `MessageAdded`, `CompressionStarted` / `CompressionFinished`, `ContextUsage`, `ToolStarted` / `ToolFinished`, `Usage`, `Finished(RunResult)`
- `RunResult`: `messages` (added this round, sliced from `start_index`) / `stop_reason` / `usage`; `StopReason` has `Completed` / `MaxStepsReached` / `Cancelled`, and **currently only `Completed` is produced**

**Context compaction** (this is the easiest part to break):

- `should_schedule_compression` sets `compression_pending` when this round's `(input + output) >= limit * trigger_ratio` (default `0.80`), using a fixed-point integer comparison scaled up by 10000 to avoid floating-point precision issues
- `active_messages()` finds the **last** `ContextSummary` and keeps only the system message at the start + the messages from the summary onward
- `compress_context()` appends the compression instruction as a system message and requires the model to output a plain-text summary; only a non-empty summary with no tool_calls counts as success, otherwise it reports `CompressionError`
- The summary content has a **structural contract**: `COMPACTION_TEMPLATE` defines XML blocks (`current_goal` / `hard_constraints` / `decisions` / `progress` / `open_questions` / `compacted_range`), and `compress_context` appends it after the caller's `compression_instruction`. The template **does not contain** `next_step` and explicitly forbids extrapolation -- this fixes "the compactor fabricating a next step the user never mentioned" (`docs/compaction.md` 5.3). **The template is configurable**: `CompressionConfig.template` (defaults to `COMPACTION_TEMPLATE`), overridable by the application layer via the `[compression]` table
- **The compaction strategy is configurable**: `CompressionConfig { trigger_ratio, retain_ratio, template }` (defaults `0.80` / `0.20` / `COMPACTION_TEMPLATE`), passed in via the `Agent` builder's `compression_config`; both `retain_budget` and `should_schedule_compression` read it. The SDK only manages the strategy parameters; the domain trade-offs stay in `compression_instruction` (see `docs/compaction.md` supplement to section six)
- After a successful compaction, `CompactParts::rebuild` rebuilds `self.messages`: **the system prompt is not kept, not copied** -- it is held separately by the `Agent` (the `system_prompt` field), and each rebuild regenerates one via `system_message()` and puts it on top (the function-form prompt is **re-resolved**, so the latest state of the working directory / project guide is reflected). `rebuild` only returns `[new ContextSummary] + current_task + remain`
- A compaction failure aborts the entire stream, and the UI shows it as an error

---

**4. Application layer (TUI and Desktop)**

- `src/main.rs`: reads `.env` (`dotenvy`), selects `Mode::Tui` / `Mode::Desktop` based on `--desktop` / `SHIRLEY_INTERFACE=desktop`, then hands off to `bootstrap::AgentFactory::assemble`. **The assembly logic has been extracted from `main.rs` into `src/bootstrap.rs`** -- `AgentFactory` is the shared LCA of the TUI and the desktop interface (the other LCA is the SDK's `Agent`); both interfaces get the same assembly product and merely render it differently. `AgentFactory::assemble` does this: loads configuration via `settings::Settings::load_default()` (**no longer scattered env reads**, see the next item), builds the `ModelConfig` (`stream(true)` / `thinking(true)` / `reasoning_effort("low")`), computes `needs_login = !settings.is_configured()` (missing config does not block startup), and hands the model config / system prompt / working directory / session directory (`src/session.rs`'s `FileSessionCatalog`, multi-session layout `.shirley/sessions/<name>.jsonl`) together with that flag to `interface::run`. **The factory's product is "the ability to create an `Agent`", not a single `Agent`** (decision 5): `build_agent(messages)` creates an anonymous `Agent` each time (tools rebuilt on demand, see below) -- multiple sessions means multiple of them; session restore is done by the caller first `store.load()` then `build_agent(log)` (an empty `Vec` = a brand-new session). At startup it **lazily opens an empty session** (rather than restoring the most recently modified old session): at this point it only decides the session name and **does not persist**; the file is really created when the first message is sent (`create_lazy`, Codex's UX); the old single-file `.shirley/session.jsonl` is adopted by `adopt_legacy` (session persistence, see `docs/session.md`). The working directory is decided by `prompt::workspace_root()` (`SHIRLEY_WORKSPACE` first, otherwise the current directory) -- it **deliberately does not belong to `settings`** (it is "which project to run in", session-level information, not "who the user is", configuration)
- `src/settings.rs`: **configuration loading (scheme A)**. It decouples "where configuration comes from" from "how it is used", purely application-layer, not touching the SDK. Priority `built-in defaults < global config.toml < workspace .shirley/config.toml < environment variables`. The config file is TOML; the `[provider]` table contains `protocol` / `base_url` / `api_key` / `models_url` / `model` / `context_window_tokens` (all `Option`, `deny_unknown_fields`). The global path is `<config_dir>/shirley/config.toml`, the workspace path is `<root>/.shirley/config.toml`. The environment-variable layer (`LOCAL_*`) is placed **last** to stay compatible with the existing `.env` habit -- old users need zero changes. `Settings::load`'s environment reads use an injected closure (tests do not touch global env); only `load_default` uses the real `config_dir` + process environment. A missing `base_url` **no longer errors and does not block startup**: `finalize` turns it into an empty string, `Settings::is_configured()` returns `false`, and the application layer accordingly automatically enters the `/login` flow in the TUI to guide the user to fill it in (see the next item). **It can also write**: `save_provider(path, provider)` reads first, then modifies, then writes (only overwriting the passed fields, not touching other keys, auto-creating the directory, and `chmod 600` after writing to tighten permissions); it is the persistence entry point for `/login`
- `src/prompt.rs`: application-side system-prompt construction. `build(working_dir)` returns a `SystemPrompt` function that, on each resolution, reads the current working directory and the workspace's `Agent.md` and assembles "persona + workspace root + project guide". The working directory is explicitly told to the model (`plan.md`: an AI that does not know the workspace will wander from the root directory), and `Agent.md` provides how the project runs and how the code is organized. **Copy and code are separated**: the body text is in `prompts/system.md` / `project_guide.md` / `project_guide_missing.md`, and `src/prompt.rs` only `include_str!`s the template + fills the `{{working_dir}}` / `{{project_guide}}` placeholders (renderer `src/prompt_template.rs`)
- `src/session.rs`: application-side session storage and the **session catalog** (`docs/session.md`). **The session contract and implementation are both in the application layer** -- the `SessionStore` trait (`append` / `load` / `truncate`, synchronous signatures) and `SessionError` (`thiserror`, `Io` / `Backend`) are defined here, and the SDK no longer exports them. `JsonlSessionStore` persists the **full raw Message log** line by line as JSONL. **No system** (regenerated on restore). **Persistence is event-driven by the application layer**: the SDK only emits `MessageAdded`, and `Session::apply_event` does `store.append` on receipt (the TUI / desktop share this path). `truncate` writes a temp file first then atomically replaces, and reopens the append handle (note: the internal `rewrite_locked` must be called while already holding the lock, otherwise the non-reentrant lock deadlocks). Multi-session (`/session`): the `SessionCatalog` trait (`list` / `open` / `create` / `create_named` / `create_lazy` / `rename` / `delete`, symmetric with `ModelCatalog`; the last four have default impls so backends without metadata support need no changes) + `FileSessionCatalog` (one session = `.shirley/sessions/<name>.jsonl`, a directory scan lists them, `adopt_legacy` adopts the old single file) + `SessionEntry { name, label, preview, modified_ms, turns }` (`preview` takes the first 40 chars of the first user message, `modified_ms` = file mtime for descending order, `turns` = number of user turns) + `EmptySessionCatalog` (`App::new` fallback). **The title is persisted as a sidecar `<name>.title`** (plain text, not polluting the Message-only JSONL); `rename` **only changes the title, not the filename / content** (Codex `/rename` semantics, sidestepping the problem of an already-open handle pointing at the old inode), and `delete` deletes the log and best-effort deletes the sidecar. **Lazy creation**: `create_lazy` returns a `LazySessionStore` -- it fixes the name at construction but **does not persist**; it materializes on the first `append` (first message) (writes the JSONL + the title sidecar); before materialization, `load` / `truncate` are treated as empty / no-ops. This way "opening the app without chatting" does not leave an empty session file
- `src/interface/selection.rs`: **mouse selection** (no dedicated doc). Left button down records an anchor, dragging extends, releasing writes the selected text to the system clipboard (`arboard`) -- **the whole thing uses only the mouse, no copy key needed**. The selection is represented with screen-cell coordinates; `highlight` overlays reverse video on the buffer (rendered on top of everything); `text` extracts text by advancing by symbol display width (skipping the continuation cells of wide characters, avoiding fake spaces between CJK). `Tui::draw` keeps a snapshot of the current frame buffer when there is a selection, and extracts from it when the left button is released
- `src/interface/tui.rs`: the main loop uses `tokio::select!`, moving the **foreground session**'s `Agent` out via `SessionManager::take_agent(name)` (by session name), `tokio::spawn`ing it into a separate task, sending results back through an `mpsc` channel, and putting it back with `restore_agent(name, agent)` when done -- each session holds its own `Agent`, and a background session's context is not lost when the foreground switches. **It consumes `AgentEvent` directly** (no more `AgentUpdate` intermediate type). On receiving an event it uniformly goes through `App::apply_event` (a thin wrapper: forwards to `sessions.active_mut().apply_event` and invalidates `message_cache`) -- **the accumulation logic is converged into `Session`, so the TUI and desktop share the same event-handling path**, and the UI only reads the result
- `src/interface/app.rs`: a pure state machine, with two kinds of entries `Item::Message` / `Item::Tools`; `Role` has User / Assistant / Summary / Error; usage, context usage, and input history (up/down key replay) are all cached here; `toggle_thinking` / `toggle_tool_args` control display
- `src/interface/ui.rs`: `MessageCache` does render caching (invalidated by width); `footer_line` shows the context usage percentage and cache hit rate (single + cumulative). The message body is handed to `markdown::render` to turn into styled `Line`/`Span`. **The scroll height must be accumulated using ratatui's own line-wrap counting** (`Paragraph::line_count`, by word boundary, requires the `unstable-rendered-line-info` feature) to build up `row_offsets`; you cannot estimate with `line.width().div_ceil(width)` -- the latter rounds by character count, undercounting long mixed Chinese/English lines, making `max_scroll` too small and the latest message obscured by the bottom input box (fixed, regression test `cache_row_count_matches_paragraph_word_wrap`)
- `src/interface/markdown.rs`: the Markdown rendering layer. `parse` uses `pulldown-cmark` to converge raw markdown into a block-level AST (`Block`: Heading / Paragraph / Code / List / Quote / Table / Rule / Html), and `render` turns `Block` into ratatui `Line`/`Span`. Supports headings, bold/italic, inline code, code blocks, ordered/unordered/task lists, nested lists (hanging indent), quotes, tables, horizontal rules. Error messages do not go through markdown (to avoid symbols in an error being eaten as syntax). There are 13 unit tests covering the various syntaxes
- `src/interface/event.rs`: terminal events are `poll`ed + `read` in a separate thread and sent to the async side through an unbounded channel. Bracketed paste is enabled (`EnableBracketedPaste`), so pasted content arrives as a single `Event::Paste`; both are disabled on `Drop`. **Mouse capture is enabled** (`EnableMouseCapture`) -- drag selection is implemented by the program itself (see `selection.rs`), so copying uses only the mouse and no copy key; the capture takes over native terminal selection, which is intentional. `MouseEventKind::Moved` (movement with no button) is discarded directly in the read loop, otherwise moving the mouse makes the terminal flood events and the main loop spin redrawing
- `src/interface/update.rs`: key mapping. `Esc` (while the AI is replying -> interrupt this round; in rewind-edit state -> cancel this change; otherwise exit) / `Ctrl+C` exit, `Ctrl+T` toggle thinking display, `Ctrl+O` toggle tool-argument expansion, `Enter` submit, `Ctrl+A` / `Ctrl+E` line start/end, `Up` / `Down` history replay, `PageUp` / `PageDown` scroll up/down (equivalent to the mouse wheel, see `event.rs`). While the model picker panel is open, keys go to the panel first; while the command-candidate popover is open, `Up/Down/Left/Right` / `Tab` / `Enter` / `Esc` go to the popover first (other keys continue normal editing). `Event::Paste` goes through `App::insert_input` to insert the whole block (newlines normalized to `\n` as ordinary characters) and does not trigger sending. **Mouse events do not go here** -- they are handled directly by `tui.rs::handle_mouse` (it needs the last frame buffer and the system clipboard)
- `src/interface/command.rs`: quick commands (slash commands). They only affect the TUI and **do not touch the SDK** -- a command either expands into a prompt sent to the AI, or is a local system message, or triggers a local panel. Currently there are `/init` (expands into a prompt that generates `Agent.md`), `/model` (opens the model picker), `/rewind` (rewinds the last user message), `/session` (opens the session picker), `/login` (step-by-step login: asks base_url / api_key / model in turn). `CommandOutcome` distinguishes these cases; fuzzy matching (fuzzy + Levenshtein) gives candidates when `/` is typed, and in the popover `Up/Down` / `Left/Right` move the highlight, `Tab` / `Enter` accept the highlighted item, and `Esc` closes the popover (without touching the input)
- `src/models.rs`: the model catalog. `ModelEntry { label, value, provider }` + `trait ModelCatalog` (async `list()`, mimicking the SDK `ToolFuture` technique) + `StaticCatalog` (fallback static list) + `RemoteCatalog` (requests the OpenAI-compatible `GET /v1/models`, falls back on failure). `bootstrap.rs` derives the endpoint from `LOCAL_BASE_URL`, and `LOCAL_MODELS_URL` can override it. When `/model` opens the panel, the main loop moves the catalog into a background task to load asynchronously (not blocking the UI)
- **History rewind (`/rewind`, scheme A: only rewind the latest one)**: it directly takes the **last** user message and fills the **original text back into the input box** (entering "edit-resend state", and the input box hint switches to "Enter to resend - Esc to cancel this change"). It only does the latest one (`docs/session.md` section 1 decision 4); there is no "selection" action, hence **no selection panel**. On entering edit state, **the input box is no longer pinned to the bottom but moves in place to the rendered position of that message in the conversation** -- the render layer replaces that message with an input box outlined in cyan (above it is the conversation before it, below it the conversation after; new messages only push the bottom out of view and do not squeeze the edit box, see `ui.rs`'s `draw_rewind_edit`). Pressing Enter first rewinds the Agent and the interface to before that message, then sends it as a brand-new round; Esc cancels this change directly (clears the input, exits edit state, and neither the Agent nor the interface moves). History that has been compacted away is not in `agent.messages` and is naturally not rewindable
- **Multi-session switching (`/session`)**: a modal picker (the same style as `/model`), with the first item fixed as a "+ New session" sentinel (`SessionPicker::NEW_NAME` is an empty string) and the rest real sessions, each showing a preview of the first user message on its second line. On confirm, **orchestration is handed to `SessionManager`** (`src/interface/session.rs`, `docs/multi-session.md` decision 4) -- the sentinel goes through `create_new(None)` (`create_lazy`, lazy), and a real session goes through `switch_to(name)`: **an already-loaded session only moves the `active` pointer and directly reuses its ready `Agent`** (no rebuild, preserving background context); a not-yet-loaded one first does `store.load()` to get the full log, then starts an `Agent` via the factory's `build_agent(log)` and inserts it before switching the foreground. **The `Agent` is anonymous, identity lives on the container** (decision 4): `Session.name` is the single source of truth. After switching, `App::rebuild_items_from_agent` rebuilds the interface items entirely and resets the session-bound statistics (usage / context usage). The session catalog is a local scan (synchronous), so the main loop takes the list directly to open it, with no background task needed. The footer shows the current session name. **The TUI and desktop share the same catalog described by `FileSessionCatalog`** (`docs/session.md` section 8 decision 7)
- **Step-by-step login (`/login`, scheme A: step-by-step Q&A version)**: **when the model service is not configured (missing `base_url`), startup automatically enters this flow** -- `bootstrap.rs` computes `needs_login` and passes it to `interface::run`, and `Tui::new` accordingly calls `App::start_login()`, turning "missing config" from a startup error into a guided step inside the TUI; the user can press Esc to cancel, and after canceling can still manually run `/login`. It does not build a form overlay, but **reuses the existing input box and `submit` path** -- after entering, the input box is temporarily commandeered as a field editor, one Enter per step. The order is fixed `base_url -> api_key -> model`, each step pre-filled with the current config (`agent.model_config()`), so pressing Enter directly means "keep unchanged"; if `base_url` is empty it stays on this step and re-asks (an empty endpoint is meaningless), and an empty `api_key` means "no auth" (common for local services, and it will **explicitly clear** the old key, unlike "keep the old value"). After finishing, it first **hot-updates the Agent** (`set_provider` + optionally `set_model`), then **persists** to the workspace `<root>/.shirley/config.toml` (`settings::save_provider`, read first then modify then write, not touching other keys, `chmod 600` after writing). If the hot update succeeds but persistence fails, it says so honestly (does not pretend to succeed). `Esc` cancels login (only leaves login state, does not exit the program, and does not change the config). The render layer is unchanged -- the prompt text goes through the existing `command_hint` (`ui.rs::input_hint` already consumes it), and in login state `on_input_changed` keeps the hint just like in rewind-edit state. The login flow is in-memory state (`App::login: Option<LoginFlow>`), and it is not persisted until finished. Tests use `set_config_path` to inject a temp path, avoiding polluting the real workspace
- `src/interface/app.rs`: the input editing state machine. `insert_input` supports pasting multi-line text (CRLF/CR normalized to LF), and the cursor always lands on a character boundary
- `src/interface/ui.rs`: the input box soft-wraps by display width and preserves hard newlines (`wrap_input`); the box height grows with the content (capped at `INPUT_MAX_LINES = 10`), and beyond that it scrolls vertically internally, keeping the cursor visible when pasting long text

**Existing tools**:

- `bash` (`src/tools/bash.rs`): first does a `split_whitespace` blacklist check (`rm` / `shutdown` / `reboot`, only a temporary fallback), then **runs through the process sandbox** (`SandboxSpec::new("bash").arg("-c").arg(&command)`), default 60s timeout. The sandbox result is rendered with stdout / stderr / exit code / timeout hint. **The real boundary is the sandbox backend, not the blacklist**
- `read_file` (`src/tools/read.rs`): **controlled file reading**, used to replace raw `cat` and cap context usage at the source. Key constraints:
  - **Path constrained by the workspace**: validated via the SDK's `WorkSpace::resolve`, rejecting absolute paths and `..` escapes (out-of-bounds / invalid paths are classified as `ArgumentsError`, so the model can succeed by changing its writing) -- implementing `docs/roadmap.md` acceptance item 4
  - **Line count is capped**: default 200 lines, hard cap `MAX_LINE_COUNT = 2000` (an explicit request for more still only reads to the cap)
  - **Bytes are capped**: a single output is capped at `MAX_OUTPUT_BYTES = 64KB`, and a single over-long line is truncated at a character boundary
  - **Suspected secrets are rejected by default**: `.env` / `*.pem` / `*.key` / `id_rsa*` etc. (see `docs/security.md` section 3)
  - **Binary detection**: a NUL in the first 8KB means reject; use `ls` to guide directories
  - Returns **plain text with line numbers** (not JSON), and on truncation appends a hint `start_line=<next line>` for continuation -- the model can read it directly and continue
- `web_search` (`src/tools/web_search.rs`): **web search**, migrated from shiwen (`shiwen-open-source`). It does not go through the main model route; instead it sends one bounded request to DeepSeek's **Anthropic-compatible Messages API** (`POST {base_url}/messages`), using the server tool `web_search_20250305` to get back structured sources (`web_search_tool_result`), normalizes them, and hands them to the main model as **untrusted external data**. Key points in `docs/web-search.md`:
  - **Form**: `#[tool]` macro + **registration hook (`on_register`)**. The tool function is stateless; the client / credentials / config it needs to run are carried by `WebSearchState`, injected by `web_search_on_register` reading env at **registration time** and `ctx.insert(state)` -- "registering the tool" and "injecting the dependency" are combined into one step, so they cannot drift; `web_search_on_unregister` does `ctx.remove::<WebSearchState>()` on unregistration. Inside the function, `ctx.get::<WebSearchState>()` retrieves it (a read-only `Arc`, shared concurrently without a lock); the argument schema is auto-generated by the macro from the signature (`ToolContext` does not go into the schema). See `docs/tool-lifecycle.md`
  - The only parameter is `query` (non-empty, <=300 chars); returns JSON `{summary, query, sources[], truncated}`
  - Normalization: merge citation snippets by URL, dedupe, keep only http(s), truncate by `max_results`; **only accept structured blocks, error if missing, never fabricate sources**
  - Security: **do not follow redirects**, a 2MB response-body cap, an overall timeout (`tokio::time::timeout`)
  - Configuration is all via environment variables (`DEEPSEEK_API_KEY` required, the rest have defaults); **when unconfigured the tool is not registered**, so the model cannot see it

> The `read` tool was once removed in commit `aaf0081` (the judgment at the time was "one bash is enough"), but raw `cat` has no output cap and easily floods the context. It has now been **reintroduced** in the form of `read_file`, with workspace constraints, line/byte caps, and secret redaction added. The plan is to add `apply_patch` later (see the root `plan.md`).

**Desktop interface (`docs/desktop-interface.md`, M0-M3 landed)**: a Tauri 2 desktop interface that **coexists** with the TUI, bringing shiwen's chat UI (React/TS + Tailwind 4) in. The two are **sibling interfaces sharing an LCA** -- not "a web layer stacked on top of the TUI": `interface/` is full of ratatui-specific logic, the webview is a separate window, and the two cannot draw the same conversation at once. The shared layer is the SDK's `Agent` (`run_stream()` -> `AgentEvent` stream) + `bootstrap::AgentFactory`'s application assembly + `interface::session::SessionManager` (multi-session orchestration); the desktop side consumes the same `AgentEvent` via Tauri command/event, with its code under `src/interface/desktop/`.

- **Startup**: `cargo run` (TUI by default) is unchanged; the desktop window uses `cargo desktop` (= `cargo run --features desktop -- --desktop`, alias in `.cargo/config.toml`), or `SHIRLEY_INTERFACE=desktop cargo run --features desktop`. **Note `--features` is a cargo argument and program arguments must go after `--`**, otherwise the program will not see `--desktop` and will fall back to the TUI. The Tauri dependency is **feature-gated + optional** (the `desktop` feature), so the default build does not touch the webview toolchain.
- **Structure**: `src/interface/desktop/` (Rust: `mod.rs` facade / `wire.rs` event mapping / `shell.rs` Tauri shell) + `src/interface/desktop/web/` (React+TS+Vite+Tailwind 4 frontend; `dist` is pointed at by `tauri.conf.json`'s `frontendDist`). To build the frontend: `cd src/interface/desktop/web && npm install && npm run build`.
- **commands**: `agent_send` (starts a round; events are pushed back via `agent://event`; the optional `references` are workspace paths referenced by `@`, composed into this round's prompt by `compose_prompt`) / `agent_model_name` / `agent_list_models` / `agent_set_model` (hot-swap the model, no Agent rebuild) / `agent_search_files` (`@`-reference workspace file search, pure application-layer `workspace_search`, not touching the SDK) / `agent_cancel` (reserved). The model-switch picker sits next to the send button (`ModelSelector.tsx`, via `AiChatComposer`'s `trailingAction` slot); both the catalog and the switch forward to the Rust side's `ModelCatalog` / `Agent::set_model`, with the same semantics as the TUI's `/model`.
- **Multi-project (multi-workspace)**: as a standalone app, Shirley is not bound to a single directory -- multiple workspaces can be registered as "projects", each with its own sessions. The registry `ProjectRegistry` (`src/project.rs`) only records "where the project is", persisted to `<config_dir>/shirley/projects.json`; each project has an `AgentFactory::assemble(root)` (each reads that project's `<root>/.shirley/config.toml`), and session logs still land in each project's own `<root>/.shirley/sessions/` (traveling with the repo). `DesktopState` holds `registry: Arc<Mutex<ProjectRegistry>>` + `runtimes: HashMap<project_id, Arc<Mutex<ProjectRuntime>>>` (each `ProjectRuntime { factory, sessions }`); **runtimes are lazily loaded per project** (`ensure_runtime`). **Project id = canonical path** (globally unique), and a session name is unique only within a project, so **the frontend route key = `project::session`** (the same as the backend pump key). commands: `agent_list_projects { archived? }` / `agent_add_project` (native folder picker `tauri-plugin-dialog`, deduped by path / restore if archived) / `agent_rename_project { project, name }` / `agent_archive_project { project }` / `agent_restore_project { project }`; **"remove project" = archive**: it only sets `archived = true` + releases the in-memory runtime, and **does not delete on-disk data**, so it can be restored from the archived list. The frontend left sidebar (`SessionSidebar.tsx`) renders the multi-project grouping: switch / rename / archive (with a second confirmation) / restore from archive / new project, with the archived group collapsed by default.
- **Session management (source of truth + fan-out, isolated per project)**: each `ProjectRuntime` holds one `SessionManager` (the **same orchestrator semantics as the TUI and the same `<root>/.shirley/sessions` data**, through the same `FileSessionCatalog`). All session commands take a `project` parameter: `agent_list_sessions { project }` / `agent_current_session { project }` / `agent_new_session { project, title? }` / `agent_switch_session { project, name }` (goes through `SessionManager::create_new` / `switch_to`, the same seam as the TUI) / `agent_rename_session { project, name, title }` / `agent_delete_session { project, name }` / `agent_load_history { project }` (takes User / Assistant body text from `active_agent().messages()` into `HistoryMessageWire`). **Runtime state is isolated by `project` + session name**: `agent_send { project, text, references?, session? }` does `take_agent` / `restore_agent` by name (not relying on the `active` pointer -- a background session may have its foreground switched away while it runs), and background sessions are independent and can run in parallel; sending is rejected while the same session is running. **Events fan out by session**: each `Session` holds its own `broadcast` channel (`events: Sender<Result<AgentEvent, String>>`, capacity 256); `agent_send` feeds each event back to `SessionManager::apply_event(name, update)` (accumulating into the corresponding `Session`), then a per-session pump task pushes it to the frontend (pump key = `project::session`); `agent_subscribe { project, session }` does `subscribe` first then `snapshot` **under the same lock** (returning `SessionSnapshotWire { session, items, running }`, no loss / no duplication), and `agent_unsubscribe { project, session }` aborts that pump. **Each `AgentEventWire` variant carries `project: Option<String>` + `session: Option<String>`** (tagged by `with_route(project, session)`), and the frontend routes events to the corresponding session view accordingly. The frontend `SessionSidebar.tsx` (left sidebar) provides project grouping + session list / switch / new (optionally named) / rename (inline edit) / delete (two-click confirm); switching to a session does `openSession` (takes a snapshot to rebuild the transcript + subscribes to its events), and switching away unsubscribes. **Scope this phase**: project list / add / switch / rename / archive / restore; session list / new / switch / rename / delete; **not doing** fork / startup `--resume`.
- **`@` references** (`web/src/lib/file-mentions/`): migrated from shiwen's entity-mentions interaction skeleton, with the object **rebound to workspace files/directories** (`useFileMentions` / `FileChips` / `FileMentionPopover`); typing `@` triggers the candidate popover, selecting generates a chip, and on send the path is composed into this round's prompt. The search lives in the application layer `src/workspace_search.rs` (workspace traversal + keyword ranking, skipping `.git`/`node_modules`/`target` etc.). **A reference is not part of the Agent's external contract** -- `Message` / `AgentEvent` are unchanged, and the SDK adds no external type.
- **Event bridge**: `wire.rs` maps `AgentEvent` -> `AgentEventWire` (JSON; `web/src/types/wire.ts` is the contract). To support tool cards, the two existing variants `ToolStarted` / `ToolFinished` **had fields appended** (`arguments` / `ok` / `output` / `elapsed_ms`) -- fields appended, the TUI ignores them with `..`, zero breakage; still no derived `Serialize` (protocol differences converge at the boundary).
- **Render alignment with shiwen** (see `docs/desktop-interface.md` 4.3.1 / 4.3.2): (1) the markdown mismatch with shiwen was **not in the component** (`AiMarkdown.tsx` is byte-for-byte identical) but in `styles/index.css` missing shiwen's `@source ".../node_modules/streamdown/dist/*.js"` -- Tailwind 4 does not scan `node_modules` by default, so all of streamdown's utility classes were never generated; adding it back fixes it; (2) the chat area is a fixed width `max-w-190` (760px) centered, with `App.tsx` wrapping the transcript area + input box in a `<section>`; (3) consecutive tool calls are collected into a single `AiToolActivityDisclosure` collapsible area ("View process - N items") instead of one card per call.
- **Frontend bridge**: `web/src/lib/bridge.ts` is dual-mode (Tauri `invoke`/`listen` vs a pure-browser mock), so `npm run dev` can debug the UI standalone.
- **Stripping decisions (confirmed)**: A2UI / entity / citation / space / share / OCR / SSE wire format are all deleted; **the approval code path is kept in `AiToolExecutionCard.tsx`, but `AssistantMessage.tsx` does not render approval-state execution items in phase one**; introducing the Node/npm/Vite build chain has been approved by the user.

---

**5. Task ledger (todo)** -- `src/todo.rs` (**application layer**, `docs/todo.md`)

A companion capability to compaction: after compaction turns the conversation into a summary, the model easily forgets where it got to. The todo records "where I got to".

**Ownership correction**: the ledger **has moved from the SDK to the application layer**. "What to record, how to nag, what copy to inject" is a trade-off of the coding-agent product, not a generic fundamental capability. The SDK keeps only a business-agnostic generic seam -- `ContextProvider` (`runtime/context.rs`): the `Agent` calls it when assembling each round's request and appends the returned text as a system message **at the end**. The ledger is just one application-layer implementation of that seam.

| File | Responsibility |
| --- | --- |
| `src/todo.rs` (application layer) | `TodoStore` (`Arc<Mutex<TaskState>>`) + `TodoStatus` + `TodoUpdate` / `TodoStep` (patch input) + `TodoTool` (hand-written `Tool`, holding an `Arc<TodoStore>`) + `TodoContextProvider` (`impl ContextProvider`) + `TASK_STATE_HEADER` / `TODO_NAG_REMINDER` + unit tests |
| `src/bootstrap.rs` (application layer) | `build_agent` creates a fresh `Arc<TodoStore>` per `Agent`, registers `TodoTool`, and injects `TodoContextProvider` as `context_provider` (the two share the same `Arc`) |
| `runtime/context.rs` (SDK) | The generic seam `ContextProvider`; `agent.rs` calls it at the end of `active_messages()` |

Core design (`docs/todo.md` section 2, three core decisions):

- **The model writes, no programmatic inference**: expose a `todo` tool, and the model decides when to update (state judgment is most accurate at the AI layer).
- **Does not go into `self.messages`, injected as a system message at the end each round**: the ledger is held separately, compaction cannot touch it, and it naturally survives compaction; the injection position is chosen at **the end** because appending does not move the earlier prefix, so when the ledger is stable the prefix cache still hits (this is exactly what "keep system in place" in `responses-api.md` is for). The cost is that each time the model calls `todo`, the prefix after "that ledger system message" is invalidated -- an inherent cost of "visible every round", minimized by "put it at the end + write only the changes". All three adapters can handle a system message at any position (ChatCompletions outputs it as-is / Responses keeps it in place / Anthropic extracts it to the top-level `system`).
- **Patch semantics**: `goal` is a replacing set, `steps` is **a whole replacement** (the list is short; resending the full list each round is naturally self-correcting), `add_findings` / `add_open_questions` **append**, `clear` resets; unprovided fields stay as-is.
- **Rendered as XML** (`<task_state>` / `<goal>` / `<steps>` / `<findings>` / `<open_questions>`): an empty ledger is not injected; `&` / `<` / `>` are escaped; `MAX_RENDER_CHARS = 4000` truncates with an explicit marker when exceeded (to prevent the ledger from monopolizing the context).
- **Nag cold-start guardrail**: when an empty ledger has not been created for several consecutive rounds, inject `TODO_NAG_REMINDER` (`TODO_NAG_AFTER_ROUNDS = 3`) -- an empty ledger has no header to inject, so the model lacks a clear trigger instruction at the start. The counter is maintained by `TodoContextProvider`'s `AtomicUsize` (no longer counted by the runtime).
- **Cleared on session switch**: session switching is done by the application-layer `SessionManager` (each session holds its own `Agent` + `TodoStore`); the old session's task ledger is shelved along with its `Agent` and **naturally does not linger**.

Known gaps are in `docs/todo.md` section 7 (no persistence / no automatic cleanup / injection loses the prefix cache / the model may not use it).

---

**6. Sandbox and workspace (SDK new capabilities)**

**Sandbox** -- `crates/shirley-agent-sdk/src/sandbox/`

| File | Responsibility |
| --- | --- |
| `spec.rs` | Platform-agnostic execution intent: program + args, cwd, env, timeout, workspace_root, read/write paths, network policy (`SandboxSpec`) |
| `backend/mod.rs` | Backend abstraction `SandboxBackend` + capability self-description `Capabilities` + `SandboxError` |
| `backend/process.rs` | Bare process backend (no isolation), used only for development/testing and as a degraded baseline |
| `output.rs` | Unified result: stdout / stderr / exit_code / timed_out / isolation / **degraded** |
| `mod.rs` | Facade `Sandbox<B>`: uniformly applies the timeout, assembles the result |

Four design principles: the boundary is in "what can be touched", not the command string; deny by default (network off, env not inherited, empty write paths); the timeout is enforced by the outermost `tokio::time::timeout`, so a backend that refuses to stop is still killed; **degradation is transparent** -- constraints the backend cannot enforce are written into `output.degraded`, and the upper layer can reject the result based on it.

Currently `ProcessBackend` is used, and it honestly reports degradation (isolation not in effect). The real backends (macOS `sandbox-exec` / Linux `bwrap`) are not hooked up yet. `SandboxBackend::execute` returns `impl Future`, the trait is not object-safe, and for now it cannot be dynamically injected with `Box<dyn>`.

**Workspace** -- `crates/shirley-agent-sdk/src/workspace/mod.rs`

`WorkSpace`: `new(root)` + `resolve(input)`, resolving a relative path under the workspace root and doing out-of-bounds validation. `WorkspaceError` distinguishes `OutsideRoot` (out of bounds, rejected) and `InvalidPath` (the path itself is unusable) -- the model uses this difference to decide whether to change the path or change its writing.

---

**7. Getting started**

To run it:

```sh
cp .env.example .env   # modify as needed; .env is in .gitignore
cargo run
```

Configuration sources (priority from low to high): built-in defaults -> global `<config_dir>/shirley/config.toml` -> workspace `<root>/.shirley/config.toml` -> environment variables. The easiest is to write in `.env` (compatible with the old habit): `LOCAL_BASE_URL` (missing does not block startup; the TUI automatically enters the `/login` flow to guide completion), `LOCAL_API_KEY` (optional; local services often do not need it), `LOCAL_MODEL`, `LOCAL_PROTOCOL`, `LOCAL_MODELS_URL`, `LOCAL_CONTEXT_WINDOW_TOKENS` (optional, default 52429). You can also write a TOML config file: the `[provider]` table + `base_url` / `api_key` / `model` / `protocol` / `models_url` / `context_window_tokens`. The `DEEPSEEK_*` group is currently not referenced by the code. The `bash` tool and the system prompt both read `SHIRLEY_WORKSPACE` (workspace root, defaulting to the current directory); the system prompt also inlines `Agent.md` (the project guide) under the workspace root. Note `.env` is in `.gitignore`, and **the key inside it has leaked once, so do not commit it**.

Common commands:

```sh
cargo build                        # build
cargo test                         # run all tests
cargo test -p shirley-agent-sdk    # run only the SDK tests
cargo clippy --all-targets         # static checks
```

Test distribution (about 102 in the application layer): `markdown.rs` 13, `app.rs` 28, `ui.rs` 11, `command.rs` 10, `bash.rs` 6 (of which `reports_sandbox_degradation` is a pre-existing red test), `read.rs` 10, `session.rs` 10, `models.rs` 4, `web_search.rs` 11, `todo.rs` 7; SDK integration tests `error_contract.rs` / `sandbox_smoke.rs` / `tool_contract.rs` 6 each, `tool_lifecycle.rs` 5, `tool_context.rs` 5, `runtime_compaction.rs` 20, `system_prompt_contract.rs` 4 (the `session_contract.rs` was deleted along with the session extraction).

---

**8. Current state and known gaps**

**What already works**:

- The full ReAct loop (including streaming): `ChatCompletions` / `Responses` / `AnthropicMessages` all three protocols are adapted
- Tool registration, argument schema generation, concurrent tool calls
- Automatic context compaction (triggered at the default 80% threshold; the threshold / retain ratio / summary template are all configurable)
- usage statistics + cache hit rate (distinguishing "not reported")
- A runnable TUI: streaming incremental rendering, thinking display, tool-argument expansion, input history, scrolling, compaction status hints
- The process sandbox framework (spec / backend abstraction / degraded reporting / timeout)
- Workspace path out-of-bounds validation
- Unified error contract (`ErrorKind` / `SdkError`)
- Task ledger (**application layer**): the `todo` tool (model self-maintained) + `TodoContextProvider` injected at the end of each round (survives compaction), registered and wired per session by `bootstrap::build_agent`; under multi-session, the ledger is naturally isolated as each session has its own `Agent` / `TodoStore`. The SDK only provides the generic seam `ContextProvider`

**What is explicitly not done**:

1. ~~**Multi-protocol**~~: `ChatCompletions` / `Responses` / `AnthropicMessages` **all three are implemented** (including streaming). The Anthropic adapter is in `docs/anthropic-messages-api.md` (`x-api-key` header, `tool_result` inside user messages, `input_tokens` excluding cache so `cache_read` must be added back, streaming `input_json_delta` fragment aggregation; **all protocol differences converge in the adapter layer, `Message` is unchanged**)
2. **Tool-argument middle layer**: currently it generates OpenAI schema directly and cannot be reused across protocols
3. **Real sandbox backends**: only `ProcessBackend` (no isolation); `sandbox-exec` / `bwrap` are not hooked up
4. **Memory system**: **V1 has landed** (`docs/memory.md`, `src/memory/`). `core.md` always-resident injection + `index.md` keyword-retrieval injection (via the `ContextProvider` seam, appended at the end of each round) + an incremental curator at session end (same model + deterministic self-check, `agent.complete` one-shot call) + a temporal conflict field (`supersedes`, conflicts do not delete history). **Belongs to the application layer** (same reason as `todo.rs`); the SDK only provides the generic seam. V2/V3 (sleep learning, BM25/embedding, cross-workspace) are in `docs/memory.md` section 10
5. **Permission control / behavior restriction**: only bash's hard-coded blacklist + the sandbox, no general permission layer (the `PermissionPolicy` in `docs/security.md` has not landed)
6. **Task planning**: how to decompose long tasks and track progress is not yet designed
7. **`apply_patch` tool**: not done yet (mentioned in `plan.md`)
8. ~~Unwired configuration~~: `temperature` / `max_output_tokens` / `tool_choice` are wired up, and a new `extra_body` escape hatch is added (see `docs/sdk-gaps.md` gap-4)
9. **Unused `StopReason`**: `MaxStepsReached` / `Cancelled` are defined but never produced (there is no max_steps and no cancellation mechanism)
10. **Compaction retry**: a compaction failure aborts directly; `plan.md` mentions "retrying a compaction failure sometimes succeeds"
11. **Skills**: **P0+P1+P2(slash) landed, the rest of P2+ not landed** (`docs/skills.md`). Embracing the Agent Skills open standard (`SKILL.md` directory + frontmatter), progressive disclosure (Tier1 catalog always resident / Tier2 activated on demand / Tier3 resources read on demand). **Application-layer implementation, zero SDK change**: `src/skills/` (`parse` / `discovery` / `catalog` / `provider` / `runtime` / `tool`), wired into `CompositeContextProvider` by `bootstrap::build_agent`. **P0** injects the Tier1 catalog at the end of each round; **P1** adds `activate_skill` (with `name` limited to an enum, strips frontmatter, lists `resources`) + `SkillRuntime` (the activated set, sharing the same `Arc` with the provider) + **appending** the activated body at the end of each round (survives compaction) + `read_skill_file` (Tier3 resources read on demand, with the root pinned to `skill.root`, reusing the `read_text_range` core of `read_file`). **KV cache key constraint**: injection is always append-style -- the catalog is a stable prefix, activated blocks are concatenated after it in activation order, and old content is byte-for-byte unchanged (`runtime::SkillRuntime::render`). **P2(slash)** adds a `/<skill-name>` command entry (`CommandOutcome::ActivateSkill`; `CommandManager::resolve` / `fuzzy_match` gain a `skills: &[String]` parameter, with built-in commands taking priority; `BuiltAgent` / `Session` carry an `Arc<SkillRuntime>` handle following the memory pattern, and the App takes it from the foreground session and calls `runtime.activate` directly; no args = activate + local system hint, with args = activate + send as this round's prompt; reuses the P1 injection seam, so the KV prefix is unaffected). **Not done** (the rest of P2+): dedup activation hints, per-skill body cap tuning, `allowed-tools` enforcement, skill installer / marketplace

**Easy to fix along the way**:

- The `adapter::ModelfinishReaon` typo (missing an i, should be `ModelFinishReason`) -- **still present**
- The `todo: this currently does not handle tool-injection logic or thinking logic.` comment above `encode_request` in `chat_completions/mod.rs` is outdated (both tools and thinking are handled)
- In `runtime`, when a tool execution fails it stuffs the error `to_string()` into the tool message (there is a TODO noting "this feels a bit off")
- In `bash.rs`, the degraded hint in `render` is currently commented out, but the unit test `reports_sandbox_degradation` still asserts `[sandbox degraded]` -- **this test should be red**; either restore the rendering or change the test
- In `bash.rs`, the tool error-message format has no unified convention (there is a TODO)

---

**9. Remember before changing code**

1. **Do not break prefix stability**. Tool-definition sorting, message order, and the system-prompt position all affect the cache hit rate. The UI status bar shows this number; take a look after changing things.
2. **The semantics of `Option<u64>` are intentional**. "Not reported" and "0" must be distinguished; do not use `unwrap_or(0)` for convenience.
3. **Compaction is irreversible**. Once a `ContextSummary` is written, the earlier messages no longer participate in the request in `active_messages()`, but the original messages still stay in `self.messages` (note: compaction **rebuilds `self.messages` entirely** to be shorter, and the compacted-away original messages do not return to the working set). Be careful with the semantics of `start_index` here (`RunResult.messages` is sliced from it) -- **after an in-loop compaction you must clamp with `start_index = start_index.min(self.messages.len())`**, otherwise the slice goes out of bounds and panics (this really blew up: `range start index 10 out of range for slice of length 6`, wrapped by async_stream as `Other("task panicked...")`). Likewise, any pattern of "record an index + rebuild the underlying container midway" is fragile: the fact that the compaction branch at the start of `run_stream` does not blow up is purely an ordering dependency -- it happens **before** `start_index` is recorded; inserting any path that rebuilds `messages` after the record will hit it again.
4. **Keep the external contract small**. The `pub use` in `lib.rs` is the SDK's facade; before adding something, ask yourself: is this a fundamental capability, or business logic?
5. **Error classification is stable**. `ErrorKind` is for logs/metrics/retry decisions; the wording can change, the kind cannot be changed casually; to decide retry, look only at `is_retryable`, do not match strings.
6. **The boundary is in the sandbox, not the blacklist**. `bash`'s string blacklist is only a temporary fallback; do not treat it as a security boundary.
7. **`plan.md` and `docs/` are living documents**. `plan.md` has a lot of "why I designed it this way" thinking, which explains things better than code comments; `docs/` are the phased plans. Read them before changing the architecture.

---

One last thing, a bit of self-interest:

This project is called Shirley, but what really drives it is the line in `plan.md`: "focus on implementing one Agent application scenario first, and in the process of implementing it keep making decisions to discover which capabilities will be useful next time". **Do not abstract for abstraction's sake**. I have seen too many SDKs die on "anticipated every requirement".

When unsure, go dig through `git log`. The commit messages are written very clearly; from `macro foundation` to `context compression` to `sandbox execution framework`, the intent of every step is left behind.
