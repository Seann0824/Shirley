# docs/README.md (lines 1-57)

**Shirley technical design index**

Design notes for the runtime underneath the bot platform (SDK, tools, memory, sessions, skills). The product shape — bots, their storage layout, how to run — is in the root `README.md`. Some notes predate the bot platform and still say "TUI" or "desktop" where they mean "the interface".

**Document list**

| Document | What it solves | Priority |
| --- | --- | --- |
| `security.md` | Permission layer, bash boundary, secrets, workspace isolation | P0 |
| `sandbox.md` | Process sandbox: spec / backend / degraded / timeout / ownership of the execution strategy | P0 |
| `runtime-hardening.md` | max_steps, retry, timeout, cancellation, compaction robustness | P0 |
| `compaction.md` | Context compaction optimization: cut-point compaction + layered retention | P0 |
| `todo.md` | Task ledger `todo`: model self-maintained, task state that survives compaction / injected at the end of each round (**implemented**; the ledger lives in the application layer `crates/shirley/src/todo.rs`, the SDK only keeps the generic seam `ContextProvider`) | P0 |
| `session.md` | Session persistence and restore: application-layer SessionStore contract / rewind semantics / event-driven persistence | P0 |
| `streaming.md` | Streaming output and reasoning streaming | P1 |
| `adapter-layer.md` | Protocol adapter middle layer, tool-argument normalization, multi-protocol | P1 |
| `responses-api.md` | Responses protocol adapter: request item expansion / response decoding / streaming events / how it lands | P1 |
| `anthropic-messages-api.md` | Anthropic Messages protocol adapter: content block / thinking echo / usage semantics (**implemented**) | P1 |
| `sdk-gaps.md` | SDK capability gap fixes: tool-context injection (landed) / request-body escape hatch (landed) / tool-order preservation (not doing) | P0 |
| `sdk-refactor-0.0.3.md` | SDK 0.0.3 refactor: naming redesign (Compress→Compact, `Content`→`MessageContent`) + protocol-wire-encoding stays in adapters (encode_messages decision) / the 5-seam extension contract (`ContextProvider` / `OutboundTransform` / `ToolContext` / `ToolProgress` / `Cancellation`) / 4-batch migration path + release checklist + landed-vs-deferred status (**0.0.3 landed; rest deferred to 0.0.4**) | P0 |
| `tool-lifecycle.md` | Tool lifecycle: `ToolContext` belongs to `ToolManager` / `on_register` (created) / `on_unregister` (destroy) / `unregister` (**implemented**) | P0 |
| `tool-macro.md` | Tool macro expressiveness: function macro extended with optional companion hooks (`on_register` / `on_unregister`), lifecycle wired by the macro (**implemented**) | P1 |
| `testing.md` | SDK unit-test strategy, cache hit-rate benchmarks | P1 |
| `web-search.md` | Web search tool `web_search`: Anthropic-compatible Messages API / normalization / configuration (**implemented**) | P1 |
| `model-providers.md` | Host-local provider profiles, manual model lists, credentials, per-chat selection, and manual acceptance steps | P1 |
| `memory-sync.md` | Personal memory synchronization, legacy review, local/remote chat switching, authentication, and manual acceptance steps | P1 |
| `memory.md` | Memory system: pure markdown + two-layer structure (always-resident overview auto-injected / on-demand detail retrieval) / background curator async curation / conflicts expressed via a timeline (**proposal, not landed**) | P1 |
| `skills.md` | Skills: embracing the Agent Skills open standard (`SKILL.md` directory) / progressive disclosure (catalog always resident + activate on demand + read resources on demand) / `SkillContextProvider` appended injection at the end of each round (catalog + activated body, survives compaction, KV-friendly) / application-layer implementation, zero SDK change (**P0+P1 landed**: discovery + parsing + catalog + `activate_skill` / `SkillRuntime` activation + `read_skill_file` resource reading; `/skill-name` slash etc. are P2) | P1 |
| `tool-calling.md` | Content tool-calling protocol: declare a "text tool format + parsing method" for models that do not support native tool calling / a transport mode orthogonal to `ModelProtocol` (`ToolCallingMode::{Native,Tagged}`) / built-in declarative XML protocol / adapter-layer wrapping, zero runtime change (**landed**; integration test `tests/tool_calling_content.rs`) | P1 |
| `tool-calling-refactor.md` | Content tool-calling protocol **convergence**: expose only `ToolCallingMode` externally / move the tool description into the adapter (revert `agent.rs`) / deliver the model config once / fix request encoding and parsing fault tolerance (**Steps 1-7 all landed**) | P1 |
| `multimodal.md` | Multimodal input (images): SDK owns the mechanism (a structured `ContentPart` in `Message` / per-protocol image encoding in all three adapters / a `supports_images` capability field on `ModelConfig` + send-time fail-fast guard), the application layer owns the policy (which models support images, the attach-image UI, session persistence of references not bytes) / the first case where "`Message` unchanged" has to bend (**proposal, not landed**) | P1 |


---

**Design principles that run through everything**

1. **Boundaries must not leak**. The boundary between the application layer and the SDK must be clean: a mapping like `encode_messages` ("internal Message -> protocol format") belongs on the message side, not the adapter layer; a hand-written duplicate translation like `AgentUpdate` must either be removed or be clearly limited to display trimming.
2. **Failures must be classifiable**. `ModelError = String` throws away the HTTP status code, whether it is retryable, and whether it is rate-limited. Any decision of "should I retry / should I abort" needs structured errors; this is a precondition for runtime robustness.
3. **Secure by default**. Currently `bash` executes with `bash -c` through `ProcessBackend`, which provides no filesystem or network isolation; there is deliberately no command-string blacklist (matching on the command text is trivially bypassed and only buys a false sense of safety). Tools are the Agent's hands; if the hands have no boundary, every other design is empty -- the boundary belongs in the sandbox backend.

4. **Use the right tool for the diagram**. Diagrams in the docs choose their carrier by what they express; do not force everything into one form for the sake of uniformity: **hierarchical structures** like directory / file layout use an **ASCII tree** (`├──` / `└──`) -- mermaid has no native tree syntax, and forcing it into `graph TD` is verbose and distorted; relationships like **flows, dependencies, data flow, sequences** use **mermaid** (the architecture diagram keeps a single legend at the top of `architecture.md`, explaining the semantics of boxes / solid lines / dashed lines / diamonds / `subgraph`). The criterion: is the reader trying to "see the structure" or "see the flow" -- the former is ASCII, the latter is mermaid.

The sandbox-layer landing design is in `sandbox.md`: it advances "secure by default" from "statically reject dangerous commands" to "physically unable to run outside a fake world", and is the second half of the `bash` boundary.
