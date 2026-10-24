# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> Workspace versioning: `shirley`, `shirley-agent-sdk`, `shirley-agent-sdk-macros`
> are released together and always share the same version number.

## [0.0.3] - Unreleased

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
