**Shirley technical design · SDK capability gap fixes (P0)**

This document handles three SDK issues reported by consumers. All three were verified against the code, and their common thread is: **the SDK did something on the application's behalf that the application should have decided** -- deciding the request-body shape, deciding whether a tool has state, deciding tool order.

Conclusion: **gap-1 (tool context) and gap-4 (request-body escape hatch) have landed**; **gap-2 (tool-order preservation) is decided not to be done** (see the "why not change it" in section 1). The two that landed are breaking changes, released as `0.0.2` (`Cargo.toml` x 3 kept in sync; the SDK's dependency on macros has its `version` changed too).

| Gap | Nature | Breaking | Landing order |
| --- | --- | --- | --- |
| gap-2 `definitions()` forcing sort-by-name | a design stance (not a defect) | -- | **decided not to do** |
| gap-4 request body hard-coded, fields not wired | a real defect, including a 400 bug | behavior change | 2 (**landed**) |
| gap-1 tools cannot get the runtime context | a real defect, largest blast radius | signature change | 3 (**landed**) |

---

**1. gap-2: `definitions()` order preservation (decided not to do)**

> **Conclusion: not doing it.** The original analysis is kept below, but the conclusion is adjusted to "keep the status quo (sort by name)".
> The reason is in 1.6: this is not a defect but a **stance** question of handing an application a low-impact decision,
> and currently nothing functionally depends on it, so the benefit of the change (migration reproducibility + model steering) does not justify
> a breaking prefix change.

**1.1 Status quo**

`tool/mod.rs`:

```rust
pub struct ToolManager {
    tools: HashMap<ToolName, Box<dyn Tool>>,
}

pub fn definitions(&self) -> Vec<&ToolDefinition> {
    let mut definitions = self.tools.values().map(|t| t.definition()).collect();
    definitions.sort_by(|a, b| a.name.cmp(&b.name));   // overstepping its bounds
    definitions
}
```

The underlying `HashMap` iteration is unordered to begin with, and then sorting by name = **doubly discarding the application's intent**.

**1.2 Why it is a defect**

The original comment's reason was "sorting for a stable prompt prefix". But **stability only requires "the same every time", not "alphabetical"**. The registration order is itself deterministic (`main.rs` is a fixed sequence of calls), and order preservation also satisfies prefix stability, while returning the order decision to the application.

An application may need to arrange tools by priority or by phase (navigation tools before write tools). The SDK's alphabetical sort would break an order the application deliberately designed, causing the prompt prefix to change and the cache hit rate to plunge.

**1.3 Plan**

Add an insertion order at the bottom layer, and have `definitions()` read by it:

```rust
pub struct ToolManager {
    tools: HashMap<ToolName, Box<dyn Tool>>,
    order: Vec<ToolName>,          // new: registration order
}
```

- `register()` on success does `self.order.push(tool_name.into())` (the duplicate check is already done earlier, so there is no repetition).
- `definitions()` reads by `order` instead of sorting.

No `IndexMap` dependency is introduced, keeping O(1) lookup.

**1.4 Semantics and impact**

- Stable = the same every time; a deterministic registration order => the prefix is still stable.
- A one-time cache miss (the order changed), then stable -- exactly what the application expects.
- The `definitions()` return type is unchanged, **invisible to callers**.

**1.5 Acceptance**

Unit test: register `c, a, b` -> the name sequence from `definitions()` must be `["c","a","b"]`, not alphabetical.

---

**1.6 Why not change it (final decision)**

Calling this a "real defect" at first **overstated** it. The retrospective:

- **The status quo is not a bug.** `sort_by(name)` produces a **deterministic** alphabetical order, the same on every request, so the prompt prefix **is already stable**. The reviewer's causal claim "changing to alphabetical -> prefix changes -> caches drop in production" **does not hold** -- order preservation is equally stable, neither path keeps dropping caches, and the only difference is the one-time miss at the switch.
- **Only two reasons for order preservation hold up**: (1) reproducing the old system's established prefix during migration (one-time value); (2) a tool's position in the array has a **weak** steering effect on the model's choice. Neither constitutes a hard requirement.
- **Currently nothing functionally depends on order**; the demand is essentially a preference.
- **The change is not zero-cost**: the bottom layer is a `HashMap` (unordered iteration), so order preservation must store an extra insertion order (`order: Vec<ToolName>` or switch to `IndexMap`), and it would **change the prompt prefix** (a breaking behavior change).

Conclusion: **keep the sort-by-name**. If a real need to "control tool order" arises in the future (e.g. ordering a multi-phase tool set by phase significantly improves selection accuracy), then do it per the plan in 1.3 -- at that point it is a **justified feature**, not the current preference.

---

**2. gap-4: request-body escape hatch + wiring the unwired fields (landed)**

> Landed implementation: `ModelConfig` gains `tool_choice` / `extra_body`; `encode_request` is changed to
> write `temperature` / `max_tokens` / `reasoning_effort` / `tool_choice` only when `Some`,
> write `thinking` as `{"type":"enabled"}` only when enabled; finally `apply_extra_body` shallow-merges
> (`null` deletes a key). Unit tests `chat_completions::tests` (5).
> Note the on-the-wire key name is `max_tokens` (ChatCompletions), not `max_output_tokens`.
> The original design derivation is kept below.

**2.1 Status quo**

`chat_completions/mod.rs::encode_request`:

```rust
let thinking = match config.thinking { true => "enabled", false => "disabled" };

let body = serde_json::json!({
    "model": &config.model,
    "messages": encode_messages(input.messages),
    "tools": encode_tools(input.tools),
    "thinking": { "type": thinking },
    "reasoning_effort": &config.reasoning_effort,
    "stream": &config.stream,
});
```

`temperature` / `max_output_tokens` are defined but not in the body; `tool_choice` is not exposed at all.

**2.2 Grading the three problems (correcting one causal claim)**

| Item | Nature |
| --- | --- |
| `thinking` hard-coded as `{"type":"enabled"}` | **a real problem, the root of the 400** -- this is a vendor-private convention, not an OpenAI standard. OpenAI only uses `reasoning_effort`; a strict endpoint would 400 on this field |
| `tool_choice` missing | **a missing feature, not a 400** -- in OpenAI it is optional and not sending it does not error; what is missing is the ability to "force/disable tool calls" |
| `temperature` / `max_output_tokens` not wired | defined but unused |

Also stuffing in `thinking` + `reasoning_effort` is "spray and pray", unreliable on strict endpoints.

**2.3 Plan: add one escape hatch, not N vendor branches**

Vendor-private fields (`enable_thinking` / `thinking_budget` / `{type:"enabled"}`...) differ endlessly; adding branches one by one is a bottomless pit. Instead, **give the application a hole to override the request body**.

`ModelConfig` adds:

```rust
#[builder(default)] pub tool_choice: Option<serde_json::Value>,
#[builder(into)]  pub extra_body: Option<serde_json::Value>,
```

`Value` rather than an enum: `tool_choice` can be `"auto"` / `"none"` / `"required"` or an object; an enum would repeatedly break the API.

`encode_request` construction order:

1. Build the standard fields: `model` / `messages` / `tools` / `stream`;
2. `temperature` / `max_output_tokens` / `tool_choice` are inserted **only when `Some`**;
3. `thinking`: output `{"type":"enabled"}` **only when `config.thinking == true`**; when `false`, **omit it entirely** (currently it would send `disabled`, a shape most endpoints do not accept);
4. Finally `apply_extra_body(&mut body, extra)`: **shallow merge**, application keys override standard keys; **a value of `null` means delete that key** -- this way the application can completely remove the SDK's default `thinking` and replace it with its own shape.

The merge function is a standalone `fn apply_extra_body(body: &mut Value, extra: &Value)`, with unit tests covering three cases.

**2.4 Semantics and impact**

- The default-path behavior stays as unchanged as possible (except `thinking:false` no longer sends `disabled`).
- Vendor-private fields all go through `extra_body`; the SDK does no vendor detection.
- Adding fields to `ModelConfig` is a builder-compatible change, invisible to callers.

**2.5 Acceptance**

Unit tests:

1. `temperature` / `max_output_tokens` / `tool_choice` appear in the body when `Some`;
2. `extra_body` overrides `reasoning_effort`;
3. `"thinking": null` in `extra_body` deletes that key;
4. when `thinking=false` the body has no `thinking` key.

---

**3. gap-1: tool runtime context injection (landed)**

> Landed implementation: `ToolContext` (`tool/mod.rs`) + `Tool::invoke` gains a `ctx` parameter +
> the macro recognizes a `ToolContext` / `&ToolContext` parameter (not entering the schema, passed to the original function in its original position).
> Regression tests `tests/tool_context.rs` (5).
>
> **Later evolution**: the old `Agent`'s `.tool_context()` builder **has been removed** -- ownership of `ToolContext`
> moved into `ToolManager`, and registration hooks `on_register` / `on_unregister` were added (injection and
> cleanup), see `docs/tool-lifecycle.md`, `docs/tool-macro.md`. The original design derivation is kept below,
> in which "option A" is the finally adopted form; **the injection path is governed by the lifecycle doc**.

**3.1 Status quo**

```rust
fn invoke(&self, input: serde_json::Value) -> ToolFuture<'_>;   // no context
```

The macro expansion (`macros/src/tool.rs`) only does `deserialize args -> super::#name(args).await`, compiling the function into a **pure function**. A tool cannot get a session, state, or any external handle.

Stateful tools (`TodoTool` holding an `Arc<TodoStore>`, an application manually doing `impl Tool + Arc<Mutex<Session>>`) **can only be hand-written, cannot use the macro**.

**3.2 Why the framework layer should handle it**

A ReAct agent's tools are naturally stateful -- tools need to read and write session state. Any stateful agent runs into this; it is not some application's special business.

**3.3 Key constraint (determines the form)**

Tools are **executed concurrently** (`runtime/agent.rs` uses `FuturesUnordered`). So the context **cannot** be a `&mut Session` -- multiple tools concurrently needing `&mut` on the same session would necessarily conflict. **The only viable form is an interior-mutability handle** (`Arc<Mutex<Session>>`). This is exactly the form applications use when working around it manually, and it is correct.

**3.4 Option A (recommended): type-erased context**

New public types:

```rust
#[derive(Clone, Default)]
pub struct ToolContext {
    inner: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ToolContext {
    pub fn new() -> Self;
    pub fn with<T: Any + Send + Sync>(self, value: T) -> Self;   // stores Arc::new(value)
    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>>;   // Arc::downcast
}
```

Signature change (`Tool` trait):

```rust
fn invoke(&self, input: Value, ctx: ToolContext) -> ToolFuture<'_>;   // new ctx, owned
```

`ToolContext` internally is `Arc<map>`, so clone = refcount +1, a negligible per-call cost; owned passing avoids lifetime tangles.

`ToolManager::invoke` passes `ctx` through; `Agent` holds the `ToolContext` (builder adds `.tool_context(ToolContext)`), and the runtime does `self.context.clone()` when calling.

The macro recognizes a parameter whose **type's last segment is `ToolContext`** as a context parameter -- it is not included in the `Arguments` struct / schema, and on expansion `ctx` is passed to the original function:

```rust
#[tool(description = "...")]
async fn save_section(
    ctx: ToolContext,
    #[param(description = "...")] input: SaveInput,
) -> Result<String, ToolError> { ... }
```

(`ToolContext` or `shirley_agent_sdk::ToolContext` both work; a function has at most one ctx parameter.)

Application side:

```rust
let ctx = ToolContext::new().with(Mutex::new(my_session));
Agent::builder().tool_context(ctx)...
```

Inside a tool:

```rust
let session = ctx.get::<Mutex<Session>>().expect("session not provided");
```

Hand-written implementations such as `TodoTool` only need to add an ignored `_ctx` parameter.

**3.5 Option B (alternative): generic `Agent<T>` / `ToolManager<T>`**

`ctx: &T` is type-safe, but the generic parameter **pollutes the whole chain** (`Agent` / `ToolManager` / `Tool` / runtime all have to carry `<T>`), and **kills `dyn Tool`** (the current `ToolManager` relies on it for type-erased storage). It conflicts with "keep the external contract small".

**3.6 Trade-off**

**Choose A.** It keeps `dyn Tool` and has a controllable blast radius; the cost is that `get::<T>()` is a runtime downcast and a miss must be handled explicitly. That is far smaller than B's contagion cost.

**3.7 Acceptance**

Integration test: register a macro tool that gets an `Arc<Mutex<Session>>` from ctx, call it **concurrently** twice, and assert the state accumulates correctly (verifying both premises "concurrent execution" and "state is available").

---

**4. Change list and breakage**

| Change | Breaking | Affected callers |
| --- | --- | --- |
| `ToolManager` adds an `order` field | no | none |
| `definitions()` return order changes | behavior | one-time prefix miss |
| `ModelConfig` adds `tool_choice` / `extra_body` | added fields (builder-compatible) | none |
| `encode_request` output changes | behavior | endpoint request body |
| `Tool::invoke` adds a `ctx` parameter | **yes** | all hand-written `impl Tool` (including `TodoTool`, application hand-written tools) |
| `ToolManager::invoke` adds `ctx` | **yes** | runtime, tests |
| add `ToolContext` | no (added) | none |
| ~~`Agent` builder adds `.tool_context()`~~ | -- | **has been removed**; `ToolContext` is now held by `ToolManager` (see `docs/tool-lifecycle.md`) |
| macro supports a ctx parameter | no (backward compatible) | none |

-> release **`0.0.2`**.

---

**5. Landing status**

| Gap | Status | Landing point |
| --- | --- | --- |
| gap-1 tool context | **landed** | `ToolContext` + `Tool::invoke` gains `ctx` + the macro recognizes a ctx parameter; tests `tests/tool_context.rs` (5). Ownership later moved into `ToolManager` (`docs/tool-lifecycle.md`) |
| gap-4 request-body escape hatch | **landed** | `ModelConfig::tool_choice` / `extra_body` + `encode_request` refactor; unit tests `chat_completions::tests` (5) |
| gap-2 tool-order preservation | **not doing** | see 1.6 |

Already synced: `Agent.md` (tool system, `#[tool]` macro, known gaps), `docs/README.md` (index).

---

**6. Version**

gap-1 and gap-4 are both breaking changes (`Tool::invoke` signature, `encode_request` output behavior),
released as **`0.0.2`** (`Cargo.toml` x 3 kept in sync).
