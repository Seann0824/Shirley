# Anthropic Messages protocol adapter

> **Status: implemented** (2025-10). `ModelProtocol::AnthropicMessages` has gone from `UnsupportedProtocol`
> to a working implementation, including streaming; 16 encode/decode unit tests + 3 streaming unit tests
> all green, and 3 live network tests (`#[ignore]`, requiring `DEEPSEEK_API_KEY`) have been run
> successfully against `https://api.deepseek.com/anthropic/v1/messages`.
>
> This document was written before the implementation, preserving the research conclusions and trade-offs
> of the time; the "open items" at the end have all been decided and landed.
>
> Relationship to Responses: `docs/responses-api.md` talks about "two expansions of the same item list",
> while this document talks about "the one with the largest structural differences among the three
> protocols". The abstraction layer itself is in `docs/adapter-layer.md`.

**Endpoint** (DeepSeek-compatible): `https://api.deepseek.com/anthropic/v1/messages`.
DeepSeek also accepts Claude model names (`claude-opus*` -> `deepseek-v4-pro`,
`claude-haiku*`/`claude-sonnet*` -> `deepseek-flash`).

**All the facts in this document come from real traffic captures** (`deepseek-flash`, 2025-10), not copied
from the spec -- several points differ from "textbook Anthropic", and are noted one by one below.

---

**1. Difference overview (vs ChatCompletions / Responses)**

| Dimension | ChatCompletions | Responses | **Anthropic Messages** |
| --- | --- | --- | --- |
| system | `messages[0]` | top-level `instructions` | top-level `system` (string or block array) |
| user message | `content: "..."` | `content:[{input_text}]` | `content:[{text}]` |
| tool call | the assistant's `tool_calls[]` | a sibling `function_call` item | a **content block** `tool_use` |
| tool result | a role=tool message | a `function_call_output` item | a `tool_result` block **inside a user message** |
| tool arguments | JSON **string** | JSON **string** | JSON **object** |
| tool schema | `function.parameters` | `parameters` | `input_schema` |
| thinking | `reasoning_content` | a `reasoning` item | a `thinking` block (with `signature`) |
| thinking send-back | optional | not sent back | can be sent back (**server does not verify signature**, see 2.4) |
| finish reason | `finish_reason` | no field, look at output | `stop_reason` |
| streaming end | `data: [DONE]` | `response.completed` | `message_stop` (no `[DONE]`) |
| streaming tool args | aggregate by `index` | final state carries the complete value | `input_json_delta` **fragment aggregation** |
| cache semantics | `prompt_tokens` **includes** cached | `input_tokens` includes cached | `input_tokens` **excludes** cached |

The three most critical:
1. **The tool result goes inside the `user` message** (not a separate role), and it is a content block;
2. **`input_tokens` excludes cache**, so the hit-rate formula is the opposite of OpenAI's;
3. **Streaming tool arguments rely on `input_json_delta` fragment aggregation** (the opposite of Responses' "final state carries the complete value").

---

**2. Request encoding**

**2.1 Top-level fields**

```jsonc
{
  "model": "deepseek-flash",
  "max_tokens": 4096,          // recommended to be required (see 2.5)
  "system": "...",               // system + ContextSummary are joined here
  "messages": [ /* see 2.2 */ ],
  "tools": [ /* see 2.3 */ ],
  "stream": true,
  "temperature": 0.3,          // optional
  "thinking": { "type": "enabled", "budget_tokens": 2048 },  // optional
  "tool_choice": { "type": "auto" }  // shape differs from OpenAI
}
```

- `system` accepts a string or a block array. We use a string (multiple system/ContextSummary joined with
  `\n\n`), consistent with the handling of Responses' `instructions`.
- `thinking` in Anthropic **is an object** (`{type, budget_tokens}`), not a string;
  this differs from Responses' `{"reasoning":{"effort":...}}` and ChatCompletions' top-level string --
  **the three shapes are all different**. We map `ModelConfig.thinking: bool` -> `{"type":"enabled"}`,
  and `budget_tokens` goes through `extra_body` (when fine-tuning is needed).
- Note that `reasoning_effort` has **no corresponding field** under the Anthropic protocol (that is a
  Responses/OpenAI concept). If the user sets `reasoning_effort`, the Anthropic encoder should **ignore
  it** rather than force it in.

**2.2 Message -> content block**

| Internal Message | Anthropic encoding |
| --- | --- |
| `System` / `ContextSummary` | lifted to the top-level `system` |
| `User{content}` | `{role:"user", content:[{type:"text", text}]}` |
| `Assistant{content, reasoning_content, tool_calls}` | `{role:"assistant", content:[thinking?, text?, tool_use...]}` |
| `Tool{tool_call_id, content}` | **`{role:"user", content:[{type:"tool_result", tool_use_id, content}]}`** |

**assistant block order**: `thinking` (if `reasoning_content` is non-empty) -> `text` (if content is
non-empty) -> each `tool_use`. Order matters: thinking must come first.

**thinking send-back**: when `reasoning_content` is non-empty, produce
`{"type":"thinking","thinking": <reasoning_content>}`.
**No `signature` is needed** -- see 2.4 (this was judged wrong at first, and corrected by measurement).

**Tool result**: a `Tool` message is encoded as a **`role:"user"`** message (Anthropic has no tool
role). Multiple consecutive Tool messages can be merged into the multiple `tool_result` blocks of one
user message, or each sent as its own user message -- the server accepts both; we **send them one by
one** (simple to implement, and the order is naturally correct).

**2.3 Tool definitions**

```json
{ "name": "bash", "description": "...", "input_schema": { /* JSON Schema */ } }
```

`input_schema` is exactly `ToolDefinition.parameters` (JSON Schema), **only the key name changes**.
Yet another confirmation: JSON Schema is the cross-protocol intermediate representation; all three
protocols consume it directly.

**2.4 The thinking signature: no send-back needed (conclusion corrected)**

Real traffic capture:
- Non-streaming: the `thinking` block directly carries a `signature` field (DeepSeek gives a UUID-like string).
- Streaming: `thinking_delta` gives the thinking text fragment by fragment, and the **last `signature_delta`** gives the signature.

**The initial (wrong) conclusion**: that "thinking must be sent back as-is, and must carry the
signature, otherwise 400", based on having captured `400 The content[].thinking in the thinking mode
must be passed back to the API.`.

**Corrected by measurement**: that 400 **has nothing to do with the signature**. Reproduced item by item
(same endpoint, same model):

| The assistant sent back | Result |
| --- | --- |
| a real tool_use id (32 chars, issued by the server) + thinking not sent back | **200** |
| a real id + thinking without signature | **200** |
| a real id + thinking + empty signature | **200** |
| a hand-forged fake id `call_1` + **thinking not sent back** | **400** (that exact error) |
| a hand-forged fake id `call_1` + thinking sent back (signature written arbitrarily) | **200** |

The criterion is the **shape of `tool_use.id`** (length / prefix), not the signature: the original 400
was because during the capture a fake id `call_1` was **hand-forged**, the server could not match it,
and only then returned that misleading hint.
As long as the id is genuinely issued by the server (always the case on the agent path), **sending
thinking back or not is both 200**.

**Therefore**: the signature **does not go into `Message`**, and does not need to be stored in the
adapter layer. `reasoning_content` (plain text) is everything needed for thinking -- it is both the
text of the "thinking block" and the content when sending back.
This version briefly added `Message::Assistant.thinking_signature`, which has been removed:

- It violates the layering -- even if the signature really needed to be sent back, that would be the
  **adapter layer's session state** ("the previous round's original thinking block"), a protocol
  implementation detail, and should not be borne by `Message`.
- And it is not even "technically necessary", so there is even less reason to pollute `Message`.

> Lesson: error messages can lie. `content[].thinking must be passed back` points the finger at
> thinking, but the real cause was the id. Locating protocol problems requires **isolating variables**
> (change only one factor at a time), rather than copying the server's error wording -- otherwise an
> adapter-layer detail gets promoted to a core model field.

**2.5 max_tokens**

In the Anthropic spec, `max_tokens` is **required**. Real traffic capture: DeepSeek's compatible
endpoint **accepts omission too** (it applies its own cap when omitted). But for cross-vendor
portability, we:
- if `max_output_tokens` has a value -> write `max_tokens`;
- if no value -> **supply a default** (e.g. 4096), rather than omitting.
This way it also works against real Anthropic. For what default to use, see "open items".

---

**3. Non-streaming response decoding**

```json
{
  "id": "...", "type": "message", "role": "assistant", "model": "deepseek-flash",
  "content": [
    { "type": "thinking", "thinking": "...", "signature": "..." },
    { "type": "text", "text": "..." },
    { "type": "tool_use", "id": "call_00_...", "name": "get_weather", "input": {"city":"Beijing"} }
  ],
  "stop_reason": "tool_use",
  "stop_sequence": null,
  "usage": {
    "input_tokens": 168,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 128,
    "output_tokens": 75,
    "service_tier": "standard"
  }
}
```

**3.1 Synthesize one assistant**: iterate over `content[]`, dispatch by `type` --
`thinking`->`reasoning_content` (+signature), `text`->`content` (concatenated),
`tool_use`->`ToolCall`. `input` is a **JSON object**, which needs `to_string()` into
`ToolCall.arguments` (the internal contract is a string). Multiple `text` blocks are concatenated directly.

**3.2 stop_reason -> ModelFinishReason**

| stop_reason | internal | note |
| --- | --- | --- |
| `end_turn` | `Stop` | normal end |
| `tool_use` | `ToolCalls` | **more robust to decide by whether content has tool_use**, but stop_reason is enough too |
| `max_tokens` | `Length` | truncated |
| `stop_sequence` | `Stop` | hit a custom stop string |
| `refusal` | `Other("refusal")` | refusal |
| `pause_turn` | `Other("pause_turn")` | long-task pause (server-side tool loop) |
| `null` | `Stop` | fallback |

Suggested to **prefer looking at whether content has `tool_use`** (the "do not read strings" principle,
consistent with Responses), with `stop_reason` as auxiliary.

**3.3 usage mapping (counterintuitive, key point)**

Decisive evidence from real traffic capture:

```
call1: input=2435 cache_read=0                    (first time, no cache)
call2: input=134  cache_read=2301                 (hit; 134 + 2301 = 2435)
```

Conclusion: **Anthropic's `input_tokens` excludes cache reads** (the **opposite** of OpenAI's
`prompt_tokens` which includes cached). So:

```
usage.input_tokens   = input_tokens + cache_read_input_tokens   // normalize into "total input"
usage.cached_input_tokens     = Some(cache_read_input_tokens)    // Some only if reported
usage.cache_reported_input_tokens = Some(total input)            // hit-rate denominator = total input
```

**This is the only one of the three protocols that needs to "add the cache back"**. If you copy
OpenAI's mapping (taking `input_tokens` directly as the total), the hit rate would compute to >100% or
be severely low. **This must be written clearly in a comment.**

`cache_creation_input_tokens` (cache write) **is not counted** in `cached_input_tokens`
(that is a "hit"; a write is not a hit), but could be considered for inclusion in the denominator of
`cache_reported_input_tokens` -- depending on whether we want to measure the "read hit rate" or "cache
effectiveness". v0 takes the former (only `cache_read`).

---

**4. Streaming decoding**

**4.1 Event table (real traffic capture)**

| Event | Payload | Handling |
| --- | --- | --- |
| `message_start` | `message{usage...}` | record the initial usage (input side) |
| `content_block_start` | `index`, `content_block{type,...}` | create a block by type: thinking / text / tool_use |
| `content_block_delta` | `index`, `delta{...}` | see 4.2 |
| `content_block_stop` | `index` | block finalization |
| `message_delta` | `delta{stop_reason}`, `usage{output_tokens}` | **the final stop_reason and output usage are here** |
| `message_stop` | -- | stream end |
| `ping` | -- | ignore |

**There is no `data: [DONE]`**; it ends with `message_stop`.

**4.2 delta types**

| delta.type | field | mapping |
| --- | --- | --- |
| `text_delta` | `text` | -> `ContentDelta` |
| `thinking_delta` | `thinking` | -> `ReasoningDelta` |
| `signature_delta` | `signature` | **ignore** (server does not verify, see 2.4) |
| `input_json_delta` | `partial_json` | **append to the current tool_use block's arguments string** |

**4.3 Tool-argument fragment aggregation (the key difference from Responses)**

Responses' final-state object carries the complete `output`, so we **do not** aggregate deltas.
**Anthropic does not work that way**: a `tool_use`'s `input` can only be assembled fragment by fragment
from `input_json_delta.partial_json`, and only after assembly is it a complete JSON string.

Aggregation key: the `index` of `content_block_start` (-> an `index -> (id, name, args_buf)` table).
On finalization (`content_block_stop` or `message_stop`), convert each unfinished tool_use into a
`ToolCall`. **Aggregate by index** (Responses uses `call_id`/`output_index`, but here `call_id` appears
only once in `content_block_start`, and `index` is the stable key within the stream).

**4.4 Finalization**

`message_delta` gives `stop_reason` and the final `output_tokens`;
`message_start` gives `input_tokens` / `cache_read`. The two must be **merged** into the final `Usage`
(streaming usage is given in two parts, whereas non-streaming gives it all at once). Finalization
produces a single `Finished(ModelResponse)`.

---

**5. How it lands**

**5.1 Integration point**: `adapter::codec` adds a `ModelProtocol::AnthropicMessages` branch, returning
`(encode_request, decode_response, decode_stream_response)`.
`invoke()` needs no changes (streaming is already brought into the codec, see `docs/responses-api.md`).

**5.2 New files**: `crates/shirley-agent-sdk/src/adapter/anthropic_messages/{mod.rs, dto.rs}`.

**5.3 Reuse**: `adapter::sse` (framing + extracting `data:`) is reused directly, shared with Responses.

**5.4 Shared code that needs changing**:
- ~~`message/mod.rs` adds `thinking_signature`~~ -- **ultimately not changed** (see 2.4;
  the signature does not need to be sent back, `Message` stays as-is, which is what the layered
  architecture should be).
- `settings.rs`: the aliases are already in place (`anthropic_messages` / `anthropic` / `messages`).

**5.5 Request headers**: Anthropic uses **`x-api-key`** (not `Authorization: Bearer`)
+ `anthropic-version: 2023-06-01`. This is the encoder layer's business, and does not affect other
protocols.

---

**6. Verification checklist**

1. **Encoding**: system / ContextSummary -> top-level `system`, not into `messages`.
2. **Encoding**: `Tool` -> a `tool_result` block in a `role:"user"` message (**not** a tool role).
3. **Encoding**: assistant block order = thinking -> text -> tool_use; thinking **without signature**.
4. **Encoding**: tool schema uses the `input_schema` key name, with the same value as `parameters`.
5. **Encoding**: the `thinking` object shape; `reasoning_effort` is ignored rather than forced in.
6. **Decoding**: the three types of `content[]` blocks synthesize one assistant; `input` object -> `arguments` string.
7. **Decoding**: full-table mapping of `stop_reason`; prefer looking at whether there is a `tool_use`.
8. **Decoding (usage)**: `input_tokens` **plus** `cache_read`; hit-rate denominator = total input.
9. **Decoding (usage)**: missing `cache_read` -> `cached_input_tokens == None` (not 0).
10. **Streaming**: `text_delta`->ContentDelta, `thinking_delta`->ReasoningDelta,
    `signature_delta` ignored.
11. **Streaming**: `input_json_delta` aggregated by index into the complete arguments.
12. **Streaming**: merge the two usage sources `message_start` + `message_delta`; `message_stop` finalizes.
13. **Streaming**: `Finished` is produced correctly even **without `[DONE]`**.
14. **Contract**: the ChatCompletions / Responses path behavior is completely unchanged (regression).

Live network tests (`#[ignore]`, requiring `DEEPSEEK_API_KEY`):
`live_anthropic_round_trip` (plain text), `live_anthropic_tool_round_trip`
(multi-round tool calling), `live_anthropic_streaming_tool_call` (streaming fragment aggregation).
All three have been run successfully against `https://api.deepseek.com/anthropic/v1/messages`.

---

**7. Decision record (decided at implementation time)**

1. ~~**`thinking_signature` into `Message`**~~ -- **revoked**. Measurement shows the signature needs no
   send-back (see 2.4), and even if it did, that would be the adapter layer's session state, and should
   not be borne by `Message`.
   `Message` stays as-is, and `reasoning_content` is everything needed for thinking.
2. **`max_tokens` default** -- **adopted 4096** (`DEFAULT_MAX_TOKENS`).
3. **`cache_creation_input_tokens` not counted in the denominator** -- **adopted** (v0 only counts `cache_read`).
4. **tool_result sent one by one** (each Tool message as one user message) -- **adopted**.
5. **`reasoning_effort` ignored** -- **adopted** (Anthropic has no corresponding field).

---

**8. Relationship to other documents**

- `docs/adapter-layer.md`: this document is the expansion of step 2 (Anthropic) of its "5. Multi-protocol
  landing", and is the third confirmation of the "tool-argument normalization" conclusion -- all three
  protocols consume JSON Schema.
- `docs/responses-api.md`: also protocol adaptation, structurally symmetric; the two documents can be
  read in parallel.
- `docs/security.md`: the sensitive marking of the `x-api-key` header (`set_sensitive(true)`) is the
  same as Bearer.

**Implementation landing point**: `crates/shirley-agent-sdk/src/adapter/anthropic_messages/{mod.rs,dto.rs}`;
`adapter::codec` adds a branch. **`Message` is unchanged** -- all protocol differences converge in the
adapter layer, which is exactly the purpose of layering. All three protocols are now implemented, and
the `codec`'s `UnsupportedProtocol` fallback branch is no longer reachable (the variant is kept for
future protocols).
