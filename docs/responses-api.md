**Shirley technical design · Responses protocol adapter**

This document distills DeepSeek's Responses API (`POST /responses`) into a landable adapter plan.
Protocol references:

- Guide: <https://api-docs.deepseek.com/guides/responses_api/>
- API: <https://api-docs.deepseek.com/zh-cn/api/create-response>

Responses and ChatCompletions are **two different wire protocols, not aliases of the same protocol**. This document only describes
"one more protocol implementation"; the refactor of the abstraction layer itself is in `docs/adapter-layer.md`.

> **Status: implemented** (`crates/shirley-agent-sdk/src/adapter/responses/`).
> Two trade-offs made at landing time:
> 1. `codec` was extended from `(Encoder, Decoder)` to `(Encoder, Decoder, StreamDecoder)`,
>    bringing streaming decoding into the abstraction too, so `invoke` no longer hard-branches by protocol;
> 2. streaming **only trusts terminal events** (`response.completed` / `incomplete` / `failed`),
>    deltas are only used for live rendering, and `function_call_arguments` fragments are not aggregated -- because the terminal object
>    already carries the full `output`, which is more reliable than piecing it together from deltas (section 4.4 "dual-track, terminal state wins").
>
> **Corrections aligned with the official OpenAI schema** (DeepSeek is essentially an OpenAI-compatible implementation):
> reasoning uses `summary` primarily and `content` as a fallback; streaming additionally recognizes
> `response.reasoning_summary_text.delta`; non-streaming `failed` turns into `AdapterError` just like streaming;
> historical assistant text uses plain-string content.

---

**1. Overview of the differences from ChatCompletions**

First, let us make clear "why we cannot reuse the existing codec". The differences are concentrated in four places:

| Dimension | ChatCompletions | Responses |
| --- | --- | --- |
| Endpoint | `POST /v1/chat/completions` | `POST /responses` |
| Messages | `messages: [{role, content}]` | `input: string \| item[]`, system stays in place as a `{role:"system"}` item |
| Tool calls | `tool_calls[]` on the assistant; results go through `role:"tool"` | top-level `function_call` / `function_call_output` items; results paired by `call_id` |
| Tool parameters | `tools[].function.parameters` | `tools[].parameters` (one less `function` wrapper) |
| Streaming end | `data: [DONE]` | **no `[DONE]`**, ends with `response.completed/incomplete/failed` |
| Token usage | `prompt_tokens` / `completion_tokens` | `input_tokens` / `output_tokens` |

**Key conclusion**: `input` is an **item list**, not a "message list". A round of conversation is
flattened here into a flat item sequence -- the assistant's text, the `function_call` it emits, and the tool-returned
`function_call_output` are **parallel sibling items**, not nested in a single assistant message.
This determines that the encoder cannot reuse a one-to-one "Message -> one wire message" mapping.

---

**2. Request encoding**

**2.1 Top-level parameters**

| Our field | Responses key | Notes |
| --- | --- | --- |
| `model` | `model` | required |
| system prompt | the `{type:"message", role:"system", content}` in input | stays in place as an ordinary item (**do not use the top-level `instructions`**: it is inserted before `input` by the server, moving a mid/late system to the very front, breaking the prefix cache) |
| `stream` | `stream` | boolean |
| `temperature` | `temperature` | range `[0, 2]`; has no effect in thinking mode |
| `max_output_tokens` | `max_output_tokens` | includes visible output + chain-of-thought tokens |
| `reasoning_effort` | `reasoning.effort` | **nested object**, values `none`/`low`/`high`/`max`; `minimal->low`, `medium`/`xhigh->high` are compatibly mapped by the server |
| `tool_choice` | `tool_choice` | `none`/`auto`/`required` or `{type:"function", name}` |
| `extra_body` | shallow merge | escape hatch, same semantics as gap-4 |

Note two pitfalls:

1. `reasoning_effort` is a **top-level string** in ChatCompletions, but in Responses it must be wrapped as
   `{"reasoning": {"effort": "..."}}`. The same `ModelConfig` field has two shapes in two protocols.
2. The key name of `max_output_tokens` here **is** `max_output_tokens` (in ChatCompletions it is
   `max_tokens`) -- which happens to match our internal field name, so do not be misled by the gap-4 mapping experience.

**2.2 Message -> input items**

Under this protocol `encode_messages` must produce an item sequence. Mapping rules:

| Internal Message | Produced item |
| --- | --- |
| `System` | `{type:"message", role:"system", content:"<string>"}`, **kept in place** |
| `ContextSummary` | same as `System` (we use it internally to carry the compaction summary, semantically a system-level instruction) |
| `User` | `{type:"message", role:"user", content: <string>}` |
| `Assistant` (with text) | `{type:"message", role:"assistant", content:"<plain string>"}` (historical text uses a string; `output_text` belongs only to **output** items, and the input side's `EasyInputMessage` does not accept it) |
| `Assistant` (with tool_calls) | each call produces a `{type:"function_call", call_id, name, arguments}` item |
| `Assistant` (both text and tool_calls) | a text item + several `function_call` items (sibling relationship) |
| `Tool` | `{type:"function_call_output", call_id, output: <string>}` |

**Key points**:

- The text content block type is `output_text` (assistant side) or `input_text` (user side). DeepSeek accepts
  both, but distinguishing them by role is more faithful to OpenAI semantics.
- The `arguments` of `function_call` is **a JSON string**, consistent with ChatCompletions, passed through directly.
- `call_id` must be non-empty and unique, and **every `function_call` must have a paired `function_call_output`**.
  Our `Message::Tool` already carries `tool_call_id`, used directly as `call_id`.
- When a single assistant has both text and tool calls, it is **split into multiple items** -- this is the most fundamental
  structural difference from ChatCompletions, and the encoder must be able to expand one-to-many.

**2.3 Tool definitions**

```json
// ChatCompletions
{ "type": "function", "function": { "name", "description", "parameters" } }
// Responses
{ "type": "function", "name", "description", "parameters" }
```

There is one less `function` wrapper layer; `name`/`description`/`parameters` are directly flattened.
`parameters` is still a JSON Schema object -- **confirming the judgment in `docs/adapter-layer.md`:
JSON Schema itself is the cross-protocol intermediate representation, and the difference is only in the wrapper layer**. Therefore tool schema
encoding only needs an `encode_tool_schema` variant, with no need to rebuild the parameter model.

Constraints: the function name is non-empty, ≤128 characters, matches `^[a-zA-Z0-9_-]+$`, and is globally unique.

---

**3. Non-streaming response decoding**

Key fields of the response object (`object: "response"`):

```json
{
  "id": "...", "object": "response", "created_at": 0,
  "status": "completed",
  "output": [
    { "type": "reasoning", "id": "rs_1",
      "content": [{ "type": "reasoning_text", "text": "..." }] },
    { "type": "message", "id": "msg_1", "role": "assistant",
      "content": [{ "type": "output_text", "text": "...", "annotations": [] }] },
    { "type": "function_call", "call_id": "fc1", "name": "take_screenshot", "arguments": "{}" }
  ],
  "usage": {
    "input_tokens": 22,
    "input_tokens_details": { "cached_tokens": 0 },
    "output_tokens": 29,
    "output_tokens_details": { "reasoning_tokens": 27 },
    "total_tokens": 51
  }
}
```

**Decoding rules**: walk `output[]`, dispatching by `type`:

- `reasoning` -> take `summary[].text` (the `summary_text` block) as `reasoning_content`;
  OpenAI-family reasoning models by default **only return `summary`** (in the schema `summary` is required, `content`
  is the optional one), and `content` (`reasoning_text`, the raw CoT) is usually empty. Implementations such as DeepSeek go through
  `content`. Take the non-empty one of the two, falling back to each other -- do not read only `content`, otherwise for OpenAI-family models
  `reasoning_content` would always be empty.
- `message` -> concatenate `content[].text` (the `output_text` blocks) as `content`.
- `function_call` -> collect as `tool_calls[]`, mapping `call_id`/`name`/`arguments` directly.

The three may appear at the same time, and are ultimately synthesized into **one** `Message::Assistant { content, reasoning_content, tool_calls }`.
This is exactly the shape that the internal model "one assistant message can carry text + reasoning + tool calls at the same time" can naturally accommodate --
the decoding direction is **many-to-one**, simpler than the encoding direction's **one-to-many**.

**finish_reason mapping** (`status` + `incomplete_details`):

| Response status | `ModelFinishReason` |
| --- | --- |
| `completed` and `output` contains `function_call` | `ToolCalls` |
| `completed` otherwise | `Stop` |
| `incomplete` and `incomplete_details.reason == "max_output_tokens"` | `Length` |
| `failed` | mapped to `AdapterError` (carrying `error`) -- non-streaming and streaming are **consistent** |
| `cancelled` / `queued` | `Other(status)`, no false `Stop` |

Note: Responses **has no `finish_reason` field**; deciding a tool call requires **looking at whether `output` contains a
`function_call` item**, not reading a string like ChatCompletions does.

**usage mapping**:

| Internal `Usage` field | Source |
| --- | --- |
| `input_tokens` | `usage.input_tokens` |
| `output_tokens` | `usage.output_tokens` |
| `cached_input_tokens` | `usage.input_tokens_details.cached_tokens` |
| `cache_reported_input_tokens` | same as above, when hit = `input_tokens` (following the "only reported counts as denominator" contract) |
| `reasoning_tokens` | `usage.output_tokens_details.reasoning_tokens` |

**Consistent with the existing contract**: when `cached_tokens` is missing, `cached_input_tokens = None` (not 0),
and `cache_hit_rate()` accordingly returns `None`. Do not `unwrap_or(0)` just for convenience.

---

**4. Streaming decoding**

**4.1 SSE shape**

Each event carries both an `event:` line and a `data:` line, and `data` also contains `type` (redundant but self-describing):

```
event: response.output_text.delta
data: {"type":"response.output_text.delta","sequence_number":11,
       "item_id":"msg_1","output_index":1,"content_index":0,"delta":"Hello"}
```

`sequence_number` increases monotonically. **The stream ends with `response.completed` / `response.incomplete` /
`response.failed`, with no `data: [DONE]`** -- the existing ChatCompletions `payload == "[DONE]"` skip logic
will never hit here, and the ending must be driven by the **event type**.

**4.2 Event table**

| Event | Our handling |
| --- | --- |
| `response.created` | ignore (or record the id) |
| `response.in_progress` | ignore |
| `response.output_item.added` / `.done` | record item boundaries (see below) |
| `response.content_part.added` / `.done` | ignore |
| `response.reasoning_text.delta` | -> `AdapterEvent::ReasoningDelta(delta)` |
| `response.reasoning_summary_text.delta` | -> `AdapterEvent::ReasoningDelta(delta)` (the **summary** increment of the OpenAI-family reasoning stream, field is `summary_index`) |
| `response.reasoning_text.done` | ignore (deltas already accumulated) |
| `response.output_text.delta` | -> `AdapterEvent::ContentDelta(delta)` |
| `response.output_text.done` | ignore |
| `response.function_call_arguments.delta` | accumulate into the arguments of the corresponding `call_id` |
| `response.function_call_arguments.done` | its complete arguments can be used for fallback validation |
| `response.completed` | -> `AdapterEvent::Finished(ModelResponse)` (including usage) |
| `response.incomplete` | -> `Finished`, `finish_reason = Length` |
| `response.failed` | -> `Err(AdapterError)`, carrying `response.error` |

**4.3 Streaming assembly of tool calls**

ChatCompletions uses `delta.tool_calls[].index` for fragment aggregation; Responses instead uses
**`call_id` + `output_index`**:

- `response.output_item.added` (item.type == `function_call`) first gives
  `call_id` / `name`, with arguments empty at this point;
- subsequent `response.function_call_arguments.delta` accumulates argument fragments
  by `call_id` (or `item_id`/`output_index`);
- at `response.output_item.done` the call is complete.

So the aggregation container changes from `BTreeMap<usize, ToolCall>` to indexing by `call_id`.
**Do not depend on arrival order** (although `sequence_number` guarantees order, do not stake correctness on it).

**4.4 The ending difference (must be handled)**

The existing `decode_stream_response` ending logic is:

```rust
if let Some(finish_reason) = finish_reason { /* yield Finished */ }
```

It **depends on ChatCompletions giving a `finish_reason` every round**. Responses has no such field, and if copied verbatim,
`Finished` would never be emitted. The correct approach is:

- when encountering `response.completed/incomplete`, **directly use the complete `response` object carried by that event**
  to construct `Finished` -- `response.output` is the authoritative result, more reliable than "piecing it together from deltas";
- deltas are only used for **live rendering**; the final state is governed by the `output` in `response.completed`.
  This is consistent with the existing design intent on line 101 of `plan.md`: "when finish_reason appears, throw the
  complete message of this round out through Finished".

> Suggestion: run the delta accumulation and the terminal object **dual-track**. If the terminal `output` disagrees with the accumulated value,
> **the terminal state wins**, and lock this behavior in tests.

---

**5. How to land it**

**5.1 Integration point**

The existing `codec(protocol)` returns an `(Encoder, Decoder)` function pair. Responses needs **three places**:

```rust
fn codec(protocol) -> Result<(Encoder, Decoder), AdapterError> {
    match protocol {
        ChatCompletions => Ok((chat_completions::encode_request, chat_completions::decode_response)),
        Responses => Ok((responses::encode_request, responses::decode_response)),
        ...
    }
}
```

But streaming decoding is currently a hard `match config.protocol` branch inside `invoke()`:

```rust
let mut stream = match &config.protocol {
    ModelProtocol::ChatCompletions => chat_completions::decode_stream_response(response).await,
    other => Err(UnsupportedProtocol)?,
};
```

**This must be refactored together**: bring streaming decoding into the codec abstraction too, otherwise every added protocol means one more hard branch.
It is suggested to extend it to a triple or a small trait:

```rust
type StreamDecoder = fn(reqwest::Response)
    -> Pin<Box<dyn Stream<Item = Result<AdapterEvent, AdapterError>> + Send>>;
```

**5.2 New files**

`crates/shirley-agent-sdk/src/adapter/responses/{mod.rs, dto.rs}`, structured symmetrically with
`chat_completions/`: `mod.rs` holds `encode_request` / `decode_response` /
`decode_stream_response`, `dto.rs` holds the deserialization structures.

**5.3 Reuse and non-reuse**

| Reuse | Do not reuse |
| --- | --- |
| `message::Message` / `ToolCall` / `Usage` | message encoding (the input item structure differs) |
| `PreparedRequest` / `ensure_success` / `apply_extra_body` | tool-definition encoding (one less `function`) |
| SSE framing logic (splitting by `\n\n`) | event dispatch and aggregation key (`call_id` vs `index`) |
| `ModelFinishReason` / `AdapterEvent` | finish determination (look at output, do not read a string) |

SSE framing can be extracted into a shared utility function, but **event semantics must be written separately for each** -- the payload shapes of the two protocols
are completely different, and forcing them into one generic decoder would only be more convoluted.

**5.4 Configuration wiring**

- `ModelProtocol::Responses` already exists; just make `codec` recognize it.
- `settings.rs` adds a `"responses"` branch when parsing `protocol` (currently only chat and anyhtopic).
- When `main.rs` constructs `ModelConfig`, if the protocol is Responses, `base_url` points to `/responses`.
- The `extra_body` escape hatch still works as usual, for vendor-private fields.

---

**6. Verification checklist**

1. **Encoding**: a conversation with system + user + assistant(text+tool_calls) + tool,
   `input` expands into the correct item sequence, and `System` stays in place in `input` as a `role:"system"` item.
2. **Encoding**: `reasoning_effort` is wrapped as `{"reasoning":{"effort":...}}`;
   the `max_output_tokens` key name is correct.
3. **Decoding (non-streaming)**: when `output` contains reasoning + message + function_call,
   synthesize one assistant with all three parts present; usage fields (including cached/reasoning) are correct.
4. **Decoding (non-streaming)**: `incomplete` -> `Length`; `failed` -> an error rather than `Other`.
5. **Streaming**: a sequence of `output_text.delta` -> a string of `ContentDelta`;
   `reasoning_text.delta` -> `ReasoningDelta`.
6. **Streaming**: `function_call` fragments are aggregated correctly by `call_id`;
   the `Finished` produced by `response.completed` is consistent with `output`.
7. **Streaming ending**: **without `[DONE]`** it still correctly emits `Finished`; a `failed` event turns into an error.
8. **Contract**: when `cached_tokens` is missing, `cached_input_tokens == None`, and
   `cache_hit_rate()` returns `None` (not 0).
9. **Regression**: the ChatCompletions path behavior is completely unchanged.

**Verification status**: items 1-9 are all covered by unit tests (`adapter::responses::tests`, 12 tests + `adapter::sse` 3 tests).
In addition there are **3 real network tests** (by default `#[ignore]`), which have been run successfully against `https://api.deepseek.com`:

```sh
export DEEPSEEK_API_KEY=...
# optional: SHIRLEY_RESPONSES_BASE_URL / SHIRLEY_RESPONSES_MODEL
cargo test -p shirley-agent-sdk --lib responses::tests::live -- --ignored --nocapture
```

| Test | Verifies | Result |
| --- | --- | --- |
| `live_responses_round_trip` | streaming plain-text round trip | `content="Hi"`, `finish=Stop`, usage correct |
| `live_responses_tool_round_trip` | non-streaming tool round trip (initiate -> return output -> finish) | both rounds correct, `call_id` pairing accepted by the server |
| `live_responses_streaming_tool_call` | streaming tool call | terminal state contains a complete `function_call`, `finish=ToolCalls` |

Two facts confirmed by real packet capture (both written into the implementation):

1. **The `output` of the streaming terminal state `response.completed` carries the complete item list** (`reasoning` +
   `function_call`, including the full `call_id` / `name` / `arguments`) -- so there is **no need** to aggregate
   `function_call_arguments.delta`; section 4.4's "terminal state wins" holds.
2. The server **does report `cached_tokens: 0`** (rather than omitting it), so `cached_input_tokens`
   will be `Some(0)`; this conforms to the contract -- `Some(0)` means "indeed hit 0", while `None` means "not reported".

---

**7. Relationship with other documents**

- `docs/adapter-layer.md`: this document is the elaboration of the **Responses-first** step in its "5. Multi-protocol landing".
  Among these, "debt 1 (relocating encode_messages)" is more urgent under this protocol -- because Responses's
  item-expansion logic is inherently unable to be shared with ChatCompletions.
- `docs/sdk-gaps.md`: gap-4's `extra_body` escape hatch continues to be used in this document.
- Line 36 of `plan.md`, "do the adapter layer first": this document is a concrete step along that route.
- `docs/anthropic-messages-api.md`: the plan for the third protocol, symmetric in structure with this document.
