# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> Workspace versioning: `shirley`, `shirley-agent-sdk`, `shirley-agent-sdk-macros`
> are released together and always share the same version number.

## [0.0.4] - 2026-11-02

> **`0.0.3` was tagged but never published to crates.io** — the registry still
> held `0.0.2` when this cut was made. `0.0.4` is therefore the first published
> package after `0.0.2` and contains the whole `0.0.3` rename pass **plus** the
> changes below. Pin `0.0.4`, not `0.0.3`.

### Added (breaking — SDK)

- **`AgentEvent::ToolFinished` gained `structured: Option<serde_json::Value>`** —
  the tool's raw return value, appended next to `output`. `output` stays the
  flattened text the *model* read; `structured` is the *human* view that the
  application layer turns into render parts (`docs/render-parts.md`). Adding a
  field to a struct variant breaks exhaustive construction/destructuring —
  match with `..`.
- **`Agent` builder field `image_resolver` + `pub type ImageResolver`** — turns
  an `ImageSource::FileId` reference into something the provider can fetch,
  resolved **at request-build time only**: `self.messages` keeps the reference,
  so the session log stays a small id instead of megabytes of base64, and a
  retry simply resolves again. Tool results are resolved (the model asked for
  them); user messages are not (the model decides what to read). An
  unresolvable reference is dropped rather than forwarded as a dangling
  provider-side id.
- **Structured tool-result envelopes** — a tool may now return either a
  `MessageContent` array carrying images, or an object
  `{ "content": <text for the model>, "parts": [<data for the UI>] }`. Only
  `content` reaches the model; `parts` is render-layer data. Any other value is
  stringified as before, so existing tools are unaffected.

### Fixed (SDK)

- **Tool calls stay paired after an interrupted turn.** A log whose tail still
  holds an orphan `tool_call` (cancelled / panicked turn) is repaired in the
  **outbound view** (`repair_tool_pairing`): unfinished calls get a filler tool
  message, and an assistant message with neither text nor tool calls is dropped.
  Healthy logs are byte-for-byte unchanged, so the prefix cache is unaffected.
  Application-side counterpart: `docs/runtime-recovery.md`.
- **Image user items derived from tool results no longer split a parallel
  group.** ChatCompletions and Responses defer such a message until the
  consecutive tool-result group ends; inserting it in the middle broke
  call/result pairing and produced upstream HTTP 400.
- **ChatCompletions accepts a `file_id` image source** instead of aborting the
  whole send with `AdapterError::Encode`: the reference is forwarded as
  `image_url.url`, so a `data:` URL (what the application's resolver produces)
  works as-is, and a bare provider id degrades to an ordinary fetch failure the
  model can report.
- **Compaction no longer tells the summarizer the model is blind.** Stripping
  images for a summary request uses its own placeholder
  (`strip_images_with` → `[image omitted from this summary request]`) instead of
  the capability one (`[image omitted: this model cannot read images]`), which
  leaked into the summary's hard constraints.

### Tests

- `crates/shirley-agent-sdk/tests/runtime_images.rs` (image references,
  resolution, file_id passthrough, capability/summary stripping) and
  `crates/shirley-agent-sdk/tests/runtime_recovery.rs` (tool pairing repair
  after an interrupted turn).

### Not in this release

- The deprecated `message::Content` alias is **still present**. Its removal —
  promised for 0.0.4 in the 0.0.3 notes — slips to `0.0.5`; migrate to
  `MessageContent` now.
- Everything else deferred by `docs/sdk-refactor-0.0.3.md` §5
  (`OutboundTransform` / `CancellationHandle` / `ToolProgress`, object-safe
  `SandboxBackend`, real `sandbox-exec` / `bwrap` backends, `tracing` feature,
  the `tool_calling.rs` split) remains unimplemented.

## [0.0.3] - 2026-10-24

The 0.0.3 release is a **rename + cleanup** pass over the SDK (see
`docs/sdk-refactor-0.0.3.md`). It does **not** change core runtime behavior;
the wire format on the wire (message `role`/`content` JSON, tool-definition
order, system-message position) is byte-for-byte unchanged, so prefix-cache
behavior is preserved.

### Changed (breaking — SDK)

- **`Compress` → `Compact` unification.** The vocabulary had drifted between
  "compression" and "compaction"; 0.0.3 standardizes on **compaction**:
  - `CompressionConfig` → `CompactionConfig`
  - `AgentEvent::CompressionStarted` → `AgentEvent::CompactionStarted`
  - `AgentEvent::CompressionFinished` → `AgentEvent::CompactionFinished`
  - `AgentError::Compression` → `AgentError::Compaction` (Display prefix is now
    `[compaction failed]`)
  - `Agent::compress_context` → `Agent::compact_context`
  - `should_schedule_compression` → `should_schedule_compaction`
  - `Agent` builder field `compression_config` → `compaction_config`,
    `compression_instruction` → `compaction_instruction`
  - prompt template `prompts/compression_instruction.md` →
    `prompts/compaction_instruction.md`
- **`message::Content` → `message::MessageContent`.** The name `Content`
  collided conceptually with `ContentPart` and the various wire-level `content`
  fields; it is renamed to `MessageContent` to make it obvious it is the
  message-body container.
  - A `#[deprecated] pub type Content = MessageContent;` alias is kept for the
    whole 0.0.3 line so downstream code keeps compiling. **The alias will be
    removed in 0.0.4** — migrate to `MessageContent` now.

### Fixed

- **`shirley-agent-sdk` failed to build in isolation.** `sandbox/backend/process.rs`
  uses `tokio::try_join!`, which requires the `macros` feature, but the crate
  declared tokio with `["process", "time", "io-util", "fs"]` only. Building the
  SDK alone (`cargo build -p shirley-agent-sdk`) errored with
  `could not find try_join in tokio`. Added the `macros` feature.

### Docs

- Added `docs/sdk-refactor-0.0.3.md` (architecture re-homing + naming redesign +
  migration path) and indexed it in `docs/README.md`.
- Corrected the stale comment in `adapter/chat_completions/mod.rs` that claimed
  `encode_messages` "belongs on the message side": protocol wire encoding is an
  adapter responsibility by design (each protocol owns its encoder), and the
  decision is recorded in the design doc.

### Deferred to 0.0.4

- Removal of the deprecated `message::Content` alias.
- `OutboundTransform` / `CancellationHandle` / `ToolProgress` seams, object-safe
  `SandboxBackend`, real `sandbox-exec`/`bwrap` backends, `tracing` feature, and
  the `tool_calling.rs` split. See `docs/sdk-refactor-0.0.3.md` §5.

## [0.0.2]

Baseline before the 0.0.3 refactor. Three protocol adapters
(`ChatCompletions` / `Responses` / `AnthropicMessages`) with streaming,
multimodal `ContentPart`, `max_steps`, `Agent::complete`, retry/`Notice` events,
and the `Content` tool-calling mode.

## [0.0.1]

Initial release.
