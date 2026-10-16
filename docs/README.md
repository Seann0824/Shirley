# docs/README.md (lines 1-57)

**Shirley technical design index**

This set of documents is a technical design proposal for the current repository (the v0.1 state described by `Agent.md`). The goal is not a rewrite, but to close the "can I use it with confidence" gaps on top of the existing three-layer structure (application layer / SDK / macro), and to put the capabilities that `plan.md` has already sketched a direction for, but that have not yet landed, into an executable order.

**Document list**

| Document | What it solves | Priority |
| --- | --- | --- |
| `architecture.md` | Architecture diagram + which node each feature lands on | Read this first |
| `storage.md` | Global configuration/state layout, project namespaces and lossless legacy migration (**implemented**) | P0 |
| `roadmap.md` | Overall roadmap: phases, dependencies, acceptance criteria | Second |
| `security.md` | Permission layer, bash boundary, secrets, workspace isolation | P0 |
| `sandbox.md` | Process sandbox: spec / backend / degraded / timeout / ownership of the execution strategy | P0 |
| `runtime-hardening.md` | max_steps, retry, timeout, cancellation, compaction robustness | P0 |
| `compaction.md` | Context compaction optimization: cut-point compaction + layered retention | P0 |
| `todo.md` | Task ledger `todo`: model self-maintained, task state that survives compaction / injected at the end of each round (**implemented**; the ledger lives in the application layer `src/todo.rs`, the SDK only keeps the generic seam `ContextProvider`) | P0 |
| `session.md` | Session persistence and restore: application-layer SessionStore contract / rewind semantics / event-driven persistence | P0 |
| `multi-session.md` | Multi-session parallelism: multiple Agents each running their own / events isolated per session (the channel is held by each Session) / per-session spawn + limit / remove `switch_session` (orchestration goes to the application layer) / same-session multiple drivers not supported (**P1 landed**: SessionManager + AgentFactory + SDK removal of switch_session; P2/P3 not landed) | P1 |
| `streaming.md` | Streaming output and reasoning streaming | P1 |
| `adapter-layer.md` | Protocol adapter middle layer, tool-argument normalization, multi-protocol | P1 |
| `responses-api.md` | Responses protocol adapter: request item expansion / response decoding / streaming events / how it lands | P1 |
| `anthropic-messages-api.md` | Anthropic Messages protocol adapter: content block / thinking echo / usage semantics (**implemented**) | P1 |
| `sdk-gaps.md` | SDK capability gap fixes: tool-context injection (landed) / request-body escape hatch (landed) / tool-order preservation (not doing) | P0 |
| `tool-lifecycle.md` | Tool lifecycle: `ToolContext` belongs to `ToolManager` / `on_register` (created) / `on_unregister` (destroy) / `unregister` (**implemented**) | P0 |
| `tool-macro.md` | Tool macro expressiveness: function macro extended with optional companion hooks (`on_register` / `on_unregister`), lifecycle wired by the macro (**implemented**) | P1 |
| `testing.md` | SDK unit-test strategy, cache hit-rate benchmarks | P1 |
| `plan.md` | Error-handling unification: completed state + follow-up task list | P0 |
| `web-search.md` | Web search tool `web_search`: Anthropic-compatible Messages API / normalization / configuration (**implemented**) | P1 |
| `desktop-interface.md` | Desktop interface: Tauri 2 + React migration of the shiwen chat UI / shared LCA / stripping shiwen custom logic (**M0-M2 landed**) | P1 |
| `memory.md` | Memory system: pure markdown + two-layer structure (always-resident overview auto-injected / on-demand detail retrieval) / background curator async curation / conflicts expressed via a timeline (**proposal, not landed**) | P1 |
| `skills.md` | Skills: embracing the Agent Skills open standard (`SKILL.md` directory) / progressive disclosure (catalog always resident + activate on demand + read resources on demand) / `SkillContextProvider` appended injection at the end of each round (catalog + activated body, survives compaction, KV-friendly) / application-layer implementation, zero SDK change (**P0+P1 landed**: discovery + parsing + catalog + `activate_skill` / `SkillRuntime` activation + `read_skill_file` resource reading; `/skill-name` slash etc. are P2) | P1 |
| `tool-calling.md` | Content tool-calling protocol: declare a "text tool format + parsing method" for models that do not support native tool calling / a transport mode orthogonal to `ModelProtocol` (`ToolCallingMode::{Native,Tagged}`) / built-in declarative XML protocol / adapter-layer wrapping, zero runtime change (**landed**; integration test `tests/tool_calling_content.rs`) | P1 |
| `tool-calling-refactor.md` | Content tool-calling protocol **convergence**: expose only `ToolCallingMode` externally / move the tool description into the adapter (revert `agent.rs`) / deliver the model config once / fix request encoding and parsing fault tolerance (**Steps 1-7 all landed**) | P1 |
| `multimodal.md` | Multimodal input (images): SDK owns the mechanism (a structured `ContentPart` in `Message` / per-protocol image encoding in all three adapters / a `supports_images` capability field on `ModelConfig` + send-time fail-fast guard), the application layer owns the policy (which models support images, the attach-image UI, session persistence of references not bytes) / the first case where "`Message` unchanged" has to bend (**proposal, not landed**) | P1 |


---

**Design principles that run through everything**

1. **Boundaries must not leak**. The boundary between the application layer and the SDK must be clean: a mapping like `encode_messages` ("internal Message -> protocol format") belongs on the message side, not the adapter layer; a hand-written duplicate translation like `AgentUpdate` must either be removed or be clearly limited to display trimming.
2. **Failures must be classifiable**. `ModelError = String` throws away the HTTP status code, whether it is retryable, and whether it is rate-limited. Any decision of "should I retry / should I abort" needs structured errors; this is a precondition for runtime robustness.
3. **Secure by default**. Currently `bash` executes with `bash -c`, and the blacklist relies on `split_whitespace` string matching, which is as good as no boundary. Tools are the Agent's hands; if the hands have no boundary, every other design is empty.

4. **Use the right tool for the diagram**. Diagrams in the docs choose their carrier by what they express; do not force everything into one form for the sake of uniformity: **hierarchical structures** like directory / file layout use an **ASCII tree** (`├──` / `└──`) -- mermaid has no native tree syntax, and forcing it into `graph TD` is verbose and distorted; relationships like **flows, dependencies, data flow, sequences** use **mermaid** (the architecture diagram keeps a single legend at the top of `architecture.md`, explaining the semantics of boxes / solid lines / dashed lines / diamonds / `subgraph`). The criterion: is the reader trying to "see the structure" or "see the flow" -- the former is ASCII, the latter is mermaid.

The sandbox-layer landing design is in `sandbox.md`: it advances "secure by default" from "statically reject dangerous commands" to "physically unable to run outside a fake world", and is the second half of the `bash` boundary.

---

**Current state baseline (measured after checking the code)**

- Code size: `src` + `crates` total 2809 lines
- Tests: 3 (all in the `read` tool), 0 on the SDK side
- clippy: 36 warnings (`shirley-agent-sdk` 32 + application layer 4)
- Known panic points: **cleared**. The unimplemented protocols in `codec` / `invoke` now return `Err(UnsupportedProtocol)` (see `docs/runtime-hardening.md`)
- Unwired configuration: **wired**. `temperature` / `max_output_tokens` / `tool_choice` go into the request body, and a new `extra_body` escape hatch is added (see `docs/sdk-gaps.md` gap-4; the `request_timeout` field has been removed)
- Unused `StopReason`: `MaxStepsReached`, `Cancelled` (`Completed` is currently the only value that is ever produced)
- Already landed (not yet done when the proposal was written): streaming output (`stream` config + `ContentDelta` / `ReasoningDelta` events + SSE incremental parsing), the TUI consuming `AgentEvent` directly
