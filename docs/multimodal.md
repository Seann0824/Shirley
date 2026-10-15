**Shirley technical design · Multimodal input (images)**

This document lands the design for "letting Shirley send image data to a multimodal model". It is a
**sibling** of `adapter-layer.md` / `responses-api.md` / `anthropic-messages-api.md`: those already
proved that "every protocol difference converges in the adapter layer, `Message` is unchanged" for
text. Multimodal input is the first case where that invariant is **no longer free** -- an image is a
structured content part, and the current `Message` model (`content: String`) physically cannot carry
it. So this document is equal parts "where the new capability lives" and "how far the
`Message`-unchanged rule has to bend".

**Scope statement**: the split is **the SDK owns the mechanism (can it be sent / how it is encoded
per protocol / the capability fact); the application layer owns the policy (should it be sent /
where the image comes from / how the user picks it)**. This follows the same line the repo already
draws for `context_window_tokens` (fact in `ModelConfig`, decision in the app), `extra_body` (escape
hatch in the SDK, values from the app), and `tool_calling` (transport mechanism in the adapter).

> **Implementation status**: **SDK mechanism landed; application-layer policy not landed.**
> The SDK now carries structured image parts (`Content` / `ContentPart::Image` / `ImagePart` /
> `ImageSource` in `message/mod.rs`), all three adapters encode them (`adapter/content.rs`), the
> capability fact lives on `ModelConfig.supports_images` (default `false`) with a send-time guard in
> `adapter::invoke`, and image references are token-estimated in `token/mod.rs`. The application
> layer has only the **minimal wiring** (`supports_images` flows from settings -> `ModelConfig`);
> there is still no attach-image UI, no `@`-image flow, and sessions persist references only. The
> decisions in section 8 are settled; the still-open application-layer work is in section 10.

---

**1. What to solve**

The trigger was a capability question: **does `deepseek-v4.1-flash` accept image input?** Answer:
**yes** -- it is DeepSeek's native multimodal model (text + image), and since the GA (2026-09-10)
vision was folded into the main model id (`deepseek-flash`), retiring the separate
`deepseek-v4-flash` / `deepseek-v4-flash-vision-exp` branches. The same family is *not* uniformly
multimodal: `deepseek-v4-flash-0731`, `deepseek-v4-flash` (0423), `deepseek-v4-pro-0813`,
`deepseek-v4-pro` (0423) are **text-only**. This heterogeneity is itself an argument for a
capability flag rather than a hard-coded assumption (section 6).

The provider's image contract (DeepSeek, OpenAI-compatible):

| Item | Value |
| --- | --- |
| Where | image parts go in the **`content` array of a `user` message**, mixed with text parts (text part first is recommended) |
| How (3 transports) | ① base64 data URL (`data:image/...;base64,...`, <= 32 MiB); ② external URL (`https://...`, URL <= 8192 chars); ③ file id (`{"type":"file","file":{"file_id":"..."}}`, after a Files API upload) |
| Optional | `detail`: `low` (downsample to 512x512) / `high` (alias `original`) / `auto` |
| Position rule | images are **only** legal in `user` messages; in `system` / `assistant` the API returns 400 |
| Not supported | video input (no V4 variant has a video modality) |
| Billing | no separate vision price; counted as ordinary Flash input tokens (usage is visible in `usage.prompt_tokens`) |

Two questions the user actually asked, which this document answers:

1. "The SDK adapter must support 3 protocols sending image data" -- **is that the app layer's job?**
2. "The model config must provide 'does it support images'" -- **is that the app layer's job?**

Short answer: **half right.** The *encoding* and the *capability fact* are **SDK** work; the *policy*
is app-layer work. Section 2 is the full split.

---

**2. Responsibility split (the core of this document)**

| Concern | Owner | Why |
| --- | --- | --- |
| Carry an image part inside a `Message` | **SDK** | `Message` is an SDK type; today `content: String` cannot hold an image. The model must change where the model is defined. |
| Encode the image per protocol (ChatCompletions / Responses / Anthropic) | **SDK** | The three `encode_*` functions live *inside* the SDK and are not reachable from the app. This is exactly the "protocol differences converge in the adapter" rule. |
| The capability fact "this model accepts images" | **SDK** (the field) | `ModelConfig` is an SDK type, and the fact is the same kind of thing as `context_window_tokens`. The SDK needs it to fail fast *before* sending. |
| Fail fast when an image is present but the model is text-only | **SDK** | The guard belongs next to the request assembly, returning a classified error (`Unsupported` / `BadRequest`), not an HTTP 400 from the wire. |
| Fill the capability fact (which model supports what) | **App layer** | The model catalog (`src/models.rs` `RemoteCatalog` / `StaticCatalog`) and `settings` are app-layer; the SDK must not hard-code a model table. |
| UI: whether to show an "attach image" entry, file picker, preview, `@`-referencing an image | **App layer** | Pure product policy. |
| Where image bytes live (upload / cache / session log) | **App layer** | Persistence and storage are app concerns (`docs/session.md`). |

One sentence: **the SDK answers "can this be sent, and how does each protocol encode it"; the
application layer answers "should this be sent, and where does the image come from".** The user's
"it's all the app layer" reading is not correct -- the mechanism and the capability fact belong in
the SDK.

---

**3. Current code state (measured)**

Everything below is **text-only** today; these are the exact seams that have to grow.

- **`Message`** -- `crates/shirley-agent-sdk/src/message/mod.rs`:
  - `System { content: String }`, `User { content: String }`, `ContextSummary { content: String }`,
    `Tool { tool_call_id, content: Option<String> }`
  - `Assistant { content: Option<String>, reasoning_content: Option<String>, tool_calls }`
  - **No variant / field can hold a structured image part.** This is the largest change surface.
  - `Display for Message` and the JSONL round-trip both assume a plain string.
- **`ModelConfig`** -- `crates/shirley-agent-sdk/src/adapter/mod.rs` (a `bon` builder): `protocol` /
  `base_url` / `model` / `api_key` / `stream` / `request_timeout` / `thinking` / `reasoning_effort` /
  `temperature` / `max_output_tokens` / `context_window_tokens` / `tool_choice` / `extra_body` /
  `tool_calling`. **No capability field.** A capability flag would sit naturally next to
  `context_window_tokens`.
- **The three encoders** (all text-only):
  - `chat_completions/mod.rs::encode_messages` -> `{"role":"user","content": <string>}`
  - `responses/mod.rs::encode_input` -> `{"role":"user","content":[{"type":"input_text","text":...}]}`
  - `anthropic_messages/mod.rs::encode_messages` -> `{"content":[{"type":"text","text":...}]}`
- **Leaves that already anticipate images** (the protocol layer left room, the SDK code did not take
  it):
  - `responses/dto.rs` comments acknowledge `EasyInputMessage` accepts
    `string | input_text/input_image/input_file`.
  - `chat_completions/dto.rs` already has an `image_tokens` usage field.
  - `chat_completions/openapi.yaml` carries a full `image_url` request example.
- **Precedent for "transform a `Message` inside the adapter"**: `adapter/tool_calling.rs`'s
  `Content(protocol)` mode already rewrites `Message::Tool` into a text feedback `Message::User`
  before encoding. Multimodal is the same family of move, but it needs a *structured* part, which is
  why `Message` itself must change (tool-calling could get away with string juggling; images cannot).

The three per-protocol image encodings to add:

```
ChatCompletions: {"type":"image_url","image_url":{"url":"data:image/png;base64,...","detail":"auto"}}
Responses:       {"type":"input_image","image_url":"data:image/png;base64,..."}   // or {"file_id": ...}
Anthropic:       {"type":"image","source":{"type":"base64","media_type":"image/png","data":"..."}}
                 // or {"type":"image","source":{"type":"url","url":"https://..."}}
```

---

**4. Design · `Message` model**

The decision that shapes everything else. Two options:

**Option A (recommended): make content structured.**

```rust
enum ContentPart {
    Text(String),
    Image(ImagePart),
}

enum ImagePart {
    DataUrl { url: String, detail: Option<ImageDetail> }, // base64 data URL
    Url { url: String, detail: Option<ImageDetail> },      // external URL
    FileId { file_id: String },                            // provider file reference
}
```

`Message::User` / `System` / `ContextSummary` / `Assistant.content` move from `String` to
`Vec<ContentPart>` (or a small `Content` newtype that is `Text` when it has one part, so the common
case stays cheap). A `From<String>` / `From<&str>` keeps existing call sites terse.

- Pros: one shape for all protocols; the encoders become a pure `ContentPart -> protocol part` map;
  no `Message` variant explosion.
- Cons: touches `Serialize`/`Deserialize`, `Display`, the JSONL session format, compaction, and
  `count_message`. The JSONL change is the riskiest (see section 7).

**Option B: keep `content: String`, add `Message::UserMultimodal { parts: Vec<ContentPart> }`.**

- Pros: smaller blast radius; text paths untouched.
- Cons: two user-message shapes; every consumer (`encode_*`, compaction, session, UI) grows a
  branch; the "one internal representation" property is lost.

Recommendation: **A**, gated behind a serialization-compat plan. The point of the SDK is to keep one
internal representation; option B trades that away to avoid a migration that has to happen anyway.

**Modeling rule**: the SDK stores an image **reference, never the pixels**. base64 is just one of the
three reference forms (`DataUrl`), and even then it is the caller's string, not a decoded buffer.

---

**5. Design · the three adapters**

Each encoder gains one arm: when a `User` message has more than one part (or any non-text part),
emit the protocol's content array; when it is a single text part, keep emitting the plain string (to
preserve byte-for-byte request shape and therefore the KV prefix cache -- see section 9).

- **ChatCompletions**: text part -> `{"type":"text","text":...}`; image -> `{"type":"image_url",
  "image_url":{"url":...,"detail":...}}`. The plain-string shortcut stays for text-only messages.
- **Responses**: the user item's `content` becomes a mix of `{"type":"input_text"}` /
  `{"type":"input_image","image_url":...}` / `{"type":"input_file","file_id":...}`. **Order
  preservation is unchanged** -- the `encode_input` "pure order-preserving mapping" rule still holds;
  we only expand *within* one user item.
- **Anthropic**: `content` blocks mix `{"type":"text"}` / `{"type":"image","source":...}`. Anthropic's
  `source.type` is `base64` or `url`; the `file_id` form is provider-specific and should map to
  whatever the endpoint accepts (for a DeepSeek-compatible endpoint, `{"type":"file","file":{...}}`).

A shared helper in the adapter layer (e.g. `encode_content_parts`) should own the
`ImagePart -> (protocol shape)` mapping so the three encoders stay thin and the difference stays in
one place. This is the same "middle layer" idea as `adapter-layer.md`.

**Guard**: before encoding, the adapter (or the runtime) checks "does the request contain any image
part, and does `ModelConfig` say images are supported?" If not, return a classified error. Doing this
in the SDK keeps the failure local and typed instead of an opaque provider 400.

---

**6. Design · the capability field**

Add to `ModelConfig` (SDK):

```rust
/// Whether the configured model accepts image input. `false` by default (deny by default).
pub supports_images: bool,
// or, if we want to future-proof for audio/video:
// pub input_modalities: Modalities,   // bitflags: TEXT | IMAGE | AUDIO | VIDEO
```

- **Default `false`** (deny by default, consistent with the sandbox/network posture).
- **Who sets it**: the application layer, when it builds the `ModelConfig` in
  `bootstrap::AgentFactory::assemble` -- either from the model catalog entry (section 7) or from
  `settings`. The SDK never hard-codes a model table.
- **Who reads it**: the SDK's send path (fail-fast guard, section 5) **and** the app layer's UI
  (show/hide the attach-image entry, warn on unsupported model).
- **Why a bitflags `input_modalities` is tempting**: it matches how OpenRouter exposes
  `input_modalities: [text, image]` and how the family is heterogeneous (some variants text-only).
  The counter-argument is YAGNI -- start with `supports_images: bool` unless audio/video is on the
  near roadmap. The repo's stated stance is "do not abstract for abstraction's sake", so **prefer the
  bool first**, keep the name stable so it can grow into a set later.

---

**7. Application layer**

- **Where the capability fact comes from**: extend `ModelEntry` (`src/models.rs`) with a modality /
  `supports_images` field. `RemoteCatalog` reads it from the OpenAI-compatible `GET /v1/models` when
  the endpoint reports it (OpenRouter does; a bare DeepSeek endpoint may not), `StaticCatalog` carries
  it in the fallback list. When the endpoint does not report it, fall back to a small built-in name
  map or to `false` (deny by default). This is the "fill the fact" responsibility.
- **UI**: an attach-image entry (TUI + desktop `@` reference already resolves workspace paths -- an
  image file is just another referenced path, so the desktop side can reuse `workspace_search`). The
  entry is shown only when `supports_images` is true.
- **Session persistence** (`docs/session.md`, `JsonlSessionStore`): the log stores the **full raw
  `Message`**, so a base64 image would be written verbatim into the JSONL. **Store references, not
  bytes**: persist the external URL / file id as-is; for a local file, persist the path (or a content
  hash + a sidecar), not the base64 blob. A base64-in-JSONL session file would be enormous and would
  re-read the whole blob on every restore. This is an app-layer decision with an SDK-format
  consequence (it constrains what `ImagePart` should hold).
- **Compaction** (`docs/compaction.md`): the summary is text. When a range being compacted contains
  images, the summary should record "an image was present here" (a placeholder / the reference), not
  drop it silently and not inline the pixels.
- **Token counting** (`token::count_message`): the heuristic text counter will under-count images.
  Either add a per-image estimate (DeepSeek exposes `image_tokens` in usage) or accept the
  under-count and let the provider's reported usage be authoritative. This affects when compaction
  triggers.

---

**8. Decisions needed before coding**

1. **`Message` shape**: Option A (`Vec<ContentPart>`) vs Option B (a new variant). Recommended: A.
2. **Serialization migration**: how the JSONL session format changes and whether old logs must keep
   loading (a `#[serde(untagged)]` or version tag). The riskiest item.
3. **Image reference modeling**: confirm `DataUrl` / `Url` / `FileId` covers what the app needs, and
   that session persistence stores references not bytes.
4. **Capability field**: `supports_images: bool` vs `input_modalities` bitflags. Recommended: bool
   first.
5. **Position validation**: enforce the "image only in a user message" rule in the SDK (fail fast) --
   confirm this is desired rather than letting the provider 400.
6. **Text-only fast path**: keep emitting a plain string when the message has exactly one text part,
   to preserve request shape and the KV prefix cache.
7. **Anthropic `file_id`**: whether to support it at all (Anthropic's native `source` is base64/url;
   `file_id` is the DeepSeek/OpenAI-compatible shape) or map it away.

---

**9. Phases and acceptance**

- **P0 -- model + capability fact**: `ContentPart` in `Message`; `ModelConfig.supports_images`; the
  send-time guard. Acceptance: a text-only request is byte-for-byte unchanged (prefix cache intact);
  an image request against a `supports_images == false` config fails with a classified error before
  the wire.
- **P1 -- three encoders**: ChatCompletions / Responses / Anthropic image encoding + a shared
  `encode_content_parts` helper + unit tests per protocol asserting the exact JSON shape.
- **P2 -- app layer**: `ModelEntry` modality field + catalog source; the attach-image UI (TUI +
  desktop); session persistence of references (not bytes); compaction placeholder; token estimate.
- **P3 -- hardening**: size / URL-length / pixel limits; non-user-position rejection; base64 size cap
  (32 MiB) checked before send; `detail` pass-through.

**Cache-stability reminder** (from `Agent.md` section 9): tool-definition order, message order, and
the system-prompt position all affect the hit rate. Expanding a text-only user message into a content
array would change its bytes and bust the prefix -- so the text-only fast path in section 5 is a
**hard requirement**, not an optimization.

---

**10. Sources**

- DeepSeek-V4.1-Flash multimodal / vision (vendor + aggregator): Apidog, 2026-09-10
  (`https://apidog.com/blog/deepseek-v4-1-flash-vision-api/`); OpenRouter model listing /
  announcement, 2026-09-16 (updated 09-29) (`https://openrouter.ai/blog/insights/deepseek-v4-vision/`).
- Internal: `docs/adapter-layer.md`, `docs/responses-api.md`, `docs/anthropic-messages-api.md`,
  `docs/session.md`, `docs/compaction.md`; code touch-points in section 3.

> **Caveat**: some per-image limits (the Vision-Exp-era 384 input-token cap per image, image-count /
> request-body / pixel caps) are carried over from the Vision-Exp documentation and are marked
> `[VERIFY]` against the official API changelog; confirm before encoding hard limits in P3.
