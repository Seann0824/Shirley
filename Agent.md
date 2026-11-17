**Shirley -- Project Agent Guide**

This document is for the next agent (or human) who takes over this repository. It describes the **current state of the code**. If you change the architecture, change this file in the same commit -- an out-of-date guide is worse than none.

---

**1. What this is**

Shirley is a **bot platform** written in Rust. One always-on server runs your bots; a phone-first web UI talks to them. The name comes from Shirley in *Code Geass*.

The loop everything serves:

> from the phone, hand a task to a bot -> it works on the machine in the background -> a delivery comes back for you to accept or return.

The interface premise: the user does not watch the agent work. They see what needs their attention (a result to judge, a failure) and nothing else.

**The concepts -- there are four, and no others.**

| Concept | What it is | Where it lives |
| --- | --- | --- |
| **Bot** | A persona (system-prompt identity block) + the capability tools it may use + the skills it may activate | `domain/bot.rs` (`BotSpec`), `~/.shirley/bots/<name>/spec.toml` |
| **Conversation** | Each bot has exactly **one** ongoing conversation, the session named `main` | `bots/<name>/sessions/main.jsonl` |
| **Task** | Work a bot hands off from its conversation (`dispatch_task`). Runs in a background session of that bot and comes back as a card in the conversation, which the user accepts or sends back | `pool/`, `interface/runtime/pool.rs`, `~/.shirley/task-pool.json` |
| **Memory** | Markdown entries. A shared root every bot reads, plus one private root per bot | `memory/`, `~/.shirley/memory/`, `bots/<name>/memory/` |

There is no workspace, no project, no session list, no desktop app, no TUI. If you find a comment that says otherwise, the comment is stale.

**Storage (`~/.shirley/`)**

```text
config.toml                 model + runtime settings
memory/                     shared memory
task-pool.json              the task queue (one for the whole platform)
artifacts/                  media library (files bots produce or users attach)
bots/<name>/spec.toml       a bot's definition (optional for built-in bots)
bots/<name>/sessions/       main.jsonl + one log per task run
bots/<name>/memory/         memory only this bot sees
bots/<name>/workspace/      the bot's working directory (bash cwd, relative paths)
```

Nothing is migrated from older layouts. The memory file format is unchanged from earlier versions.

One gap is a **blocker, not debt**: the sandbox is `isolation: none` (a bare process). An always-on agent that can read `~/.ssh` and act on your behalf needs a real backend (`bwrap` / `sandbox-exec`) and refusal of sensitive paths; see `docs/security.md`.

---

**2. Repository layout**

A virtual Cargo workspace (`resolver = "3"`, `default-members = ["crates/shirley"]`), three flat crates:

| Crate | Responsibility |
| --- | --- |
| `crates/shirley/` | The application: bots, sessions, memory, task pool, HTTP/SSE server, web UI. Binary name `Shirley`. |
| `crates/shirley-agent-sdk/` | Business-agnostic agent fundamentals: messages, tools, protocol adapters, the ReAct runtime, sandbox. Published to crates.io. |
| `crates/shirley-agent-sdk-macros/` | The `#[tool]` attribute macro. |

There are **no Cargo features**. `cargo run` is the whole product.

**Application crate (`crates/shirley/src/`)** -- lib + thin bin; the only public item is `run()`. Dependency direction points down:

```text
lib.rs / main.rs          entry: assemble the factory, start the server
bootstrap.rs / prompt.rs  AgentFactory (everything needed to build an Agent) + system prompt
interface/
  server/                 axum: POST /api/cmd/{name}, GET /api/events (SSE), static UI
  runtime/                BotRuntimeManager: per-bot runtimes, the turn driver, the pool executor
  session/                SessionManager: an Agent + its view items + its event channel
  wire.rs / view.rs       AgentEvent -> JSON DTOs; render parts
  web/                    the UI (Vite + React + TypeScript)
domain/                   bot / session / status / models / model_providers / todo
infra/                    settings / storage / artifact / remote / prompt_template
pool/                     the task-pool model (states, transitions, persistence)
memory/ skills/ subagent/ tools/ computer/    capabilities
```

`lib.rs` re-exports `domain::*` and `infra::*` at the crate root (`crate::settings`, `crate::bot`, ...).

**Web UI (`interface/web/src/`)**

```text
api/types.ts      the wire contract (mirrors wire.rs, runtime/mod.rs, pool/mod.rs)
api/bridge.ts     fetch commands + one shared EventSource
state/            hooks: useBots, useConversation, useTasks, useMemory, useWide
components/       Icon, BotAvatar, BotRow, Composer, Chrome
views/            Home, Chat, Tasks, Work, Memory, NewBot
index.css         the design-system stylesheet (from the original design)
app.css           additions for live data
```

Layout: one full-viewport column on phones; from 900px, the bot list on the left and the open view (a conversation by default) on the right. There is no device mock-up. Do not animate `transform` on a view container: the composer is absolutely positioned against its pane, and a transformed ancestor captures it.

`build.rs` runs `npm install` (first time) and `npm run build` when UI sources change, so `cargo run` serves a current page. `SHIRLEY_SKIP_WEB_BUILD=1` skips it. `dist/` is not checked in.

**Prompts live in markdown.** Every prompt body is in `crates/shirley/prompts/*.md` or `crates/shirley-agent-sdk/prompts/*.md`; Rust only `include_str!`s and fills `{{placeholders}}` (`infra/prompt_template.rs`, single pass, unknown placeholders kept verbatim). To change wording, edit the markdown.

---

**3. How a bot is assembled**

`AgentFactory::assemble()` builds the platform factory once (model config, catalogs, media library, shared memory root, the full skill catalog). `AgentFactory::for_bot(spec)` derives a bot's factory from it -- cheap, no I/O:

| Per bot | Source |
| --- | --- |
| Identity block at the top of the system prompt | `spec.system_prompt_extra` -> `{{identity}}` in `prompts/system.md` |
| Capability tools (`bash` `grep` `read` `media` `web_search`) | `spec.tools`; empty = all (`bot::platform_tools`) |
| Skills | `spec.skills`; empty = every discovered skill |
| Session directory | `bots/<name>/sessions` |
| Memory roots | `[~/.shirley/memory, bots/<name>/memory]` |
| Workspace | `bots/<name>/workspace`: `bash` cwd, base for relative paths in `read` / `grep`, named in the system prompt. `read` also accepts absolute paths (secret-file refusal still applies) |

Always registered regardless of the spec: `todo`, `recall`, `create_bot`, `activate_skill`, `read_skill_file`, `computer_*`, and `subagent` (top-level agents only).

`BotRuntimeManager::ensure_runtime(bot)` loads the spec (`spec.toml`, else a built-in default: `nono` generalist, `patch` code, `scout` research, else a generic bot of that name), derives the factory, opens `main`, and caches the runtime.

**Memory scoping** reuses the store's two-root design: an entry's `domain` decides where it is written -- `personal` -> the shared root, `project` -> the bot's private root. Reads merge both. The curator (a background model call) runs **after every turn**, because a conversation that never ends has no other moment to distil memory. Memory is never written by a tool.

**The turn driver** (`BotRuntimeManager::spawn_turn`): take the session's `Agent`, stream `run_stream`, apply each `AgentEvent` to the session (which persists `MessageAdded` to the JSONL log) and fan it out through the sink as an `AgentEventWire` tagged with `bot` + `session`; put the `Agent` back; start curation.

**Talking and doing.** The user only ever talks to a bot. Quick things the bot answers itself; longer work it hands off with `dispatch_task` (title + self-contained brief) and keeps talking. The card for that task sits where the tool call sits in the conversation and updates in place (the web UI matches the call's `task_id` against the pool). Each turn the bot is reminded of its tasks and their results (`pool::desk::TaskContextProvider`), so "add a budget to the map" becomes `task_note` on the right task. A note on a queued or running task waits in `pending_notes` and sends the finished run straight back for rework; a note on a delivered task reworks it at once. Workers are built with `build_worker_agent`, which has no desk: a worker cannot dispatch workers.

The home composer asks `agent_route` which bot should hear the message, then opens that conversation and sends it there.

**The task pool**: one `TaskPool` for the platform. `pool_pump` starts queued tasks (up to `pool::MAX_CONCURRENT`) each in a session of the task's bot, never `main`. A watcher waits for `Finished`, records a `Delivery` (the final message + artifacts added during the run), and pumps again. The server ticks the pump every 20 s, so the queue advances with no browser open. States: `to_do -> in_progress -> in_review -> done`, `blocked` on failure; `reject` re-queues with the user's note in the same session.

---

**4. The server API**

`POST /api/cmd/{name}` with a JSON object; `GET /api/events` is one SSE stream of every bot's events. `session` defaults to `main` everywhere.

| Command | Purpose |
| --- | --- |
| `agent_list_bots`, `agent_create_bot` | The bot catalog |
| `agent_subscribe {bot}` | Snapshot of the conversation + the event cursor (`sequence`) |
| `agent_send {bot, text, images?}`, `agent_cancel {bot}` | Drive a turn |
| `agent_route {text}` | Which bot should hear a message (home screen) |
| `agent_pool_list`, `agent_pool_enqueue {brief, bot?}`, `agent_pool_note {id, note}`, `agent_pool_accept/reject/retry/cancel/remove {id}` | The task pool (tasks are normally created by a bot's `dispatch_task`) |
| `agent_memory_list {bot}` | Entries the bot sees, tagged `shared` / `bot` |
| `agent_asset_url {bot, id}`, `POST /api/upload` | Media library out / in |
| `agent_asr_status`, `POST /api/transcribe[?partial=1&language=]` | Voice input: a WAV body in, text out (`infra/asr.rs`, `docs/voice-input.md`) |
| `agent_model_providers`, `agent_list_models`, `agent_set_model` | Model configuration (no UI yet) |

A client opens a conversation by subscribing, then applies stream events whose `sequence` is greater than the snapshot's. `SHIRLEY_SERVER_TOKEN` enables bearer auth (required when listening outside loopback); `EventSource` passes it as `?token=`.

When you add or change a command or an event, update `interface/web/src/api/types.ts` in the same change.

---

**5. The SDK (`crates/shirley-agent-sdk`)**

The rule: the SDK only accumulates capabilities you would still need the next time you build an agent. No product decisions. Its facade is the `pub use` list in `lib.rs`; `message` / `adapter` / `runtime` are private modules behind it.

```text
Agent::run_stream(task)
  -> compact context first if scheduled
  -> adapter::invoke -> AdapterEvent stream (deltas, then Finished)
  -> record the assistant message
  -> run tool calls concurrently, record tool messages
  -> loop until the model stops calling tools
  -> AgentEvent stream, ending in Finished(RunResult)
```

- **Messages** (`message/`): `Message` is `System` / `User` / `Assistant` / `Tool` / `ContextSummary`. `Usage` distinguishes "reported 0" from "not reported" (`Option<u64>`); do not `unwrap_or(0)`.
- **Errors** (`error.rs`): every layer's error implements `SdkError` (`kind()`, `is_retryable()`). Decide retries from `is_retryable`, never by matching message text.
- **Adapters** (`adapter/`): `ChatCompletions`, `Responses`, `AnthropicMessages`, all with streaming. Protocol differences stay here; `Message` is protocol-agnostic. Message order is never rearranged (prefix cache). `ToolCallingMode::Tagged` serves models without native tool calling.
- **Tools** (`tool/`): `Tool` trait + `ToolManager` (definitions sorted by name -- keep it, it stabilises the prompt prefix) + `ToolContext` (type-erased per-manager state). `on_register` / `on_unregister` hooks.
- **`#[tool]` macro**: turns an async fn into a tool; `#[param(description = "...")]` on every parameter; an optional `ToolContext` parameter is injected and excluded from the schema. The annotated fn and its hooks must be at the same module level.
- **Runtime** (`runtime/`): `Agent` (bon builder), compaction (`CompactionConfig`, `plan_cut`), `ContextProvider` (append text at the end of each request -- how todo, memory and skills are injected), `SystemPrompt` (string or function).
- **Sandbox** (`sandbox/`): `SandboxSpec` + `SandboxBackend`; only `ProcessBackend` exists (no isolation, reported honestly via `output.degraded`).

The SDK holds no session and no ledger. `self.messages` is its private working set; the application seeds it at construction and persists by listening to `MessageAdded`. There is no API to push or rewrite messages, on purpose.

Design notes per topic are in `docs/` (index: `docs/README.md`). Several predate the bot platform and say "TUI" or "desktop" where they mean "the interface"; the mechanisms they describe are current.

---

**6. Running and checking**

```sh
cp .env.example .env    # LOCAL_BASE_URL / LOCAL_API_KEY / LOCAL_MODEL
cargo run               # builds the UI if needed; http://127.0.0.1:8787
```

Configuration layers, low to high: built-in defaults, `~/.shirley/config.toml`, environment (`LOCAL_*`). `web_search` needs `DEEPSEEK_API_KEY`; without it the tool is not registered. `.env` is git-ignored; never commit it.

UI development: `npm run dev` in `interface/web` (port 5173, `/api` proxied to the server).

```sh
cargo test --workspace                     # all Rust tests
cargo clippy --workspace --all-targets     # lint gate
cargo fmt --all -- --check
npm test                                   # in interface/web: vitest
npm run build                              # in interface/web: tsc + vite
```

Lints: `[workspace.lints]` in the root `Cargo.toml` is the single policy; Tier-1 lints are `deny`. Silence one site with `#[expect(lint, reason = "...")]`, never a bare `#[allow]`. Rationale in `docs/rust-guidelines.md`. CI (`.github/workflows/ci.yml`) runs build, test, clippy, rustfmt, cargo-audit and the web build.

---

**7. Known gaps**

1. **Sandbox**: no isolation (see section 1).
2. **Windows**: paths are portable, but the `bash` tool needs a `bash` on `PATH`.
3. **Model settings UI**: providers and models are configured through `.env` / `config.toml`; the API exists (`agent_model_providers`) but no page uses it.
4. **Bot editing**: bots can be created (UI via NONO's `create_bot`, or by writing `spec.toml`) but not edited or deleted from the UI; hiding a bot on the home screen is a per-device preference.
5. **Routing** is one model call (`interface/runtime/route.rs`, prompt `prompts/route.md`) over each bot's persona and tools; on any failure the message goes to `nono`. It sees only the text, not attachments.
6. **Curation cost**: one extra model call per turn; not throttled.
7. **Markdown**: the chat renders assistant text as plain text.
8. **Module cycles**: `settings <-> storage`, `remote <-> memory`, `models <-> model_providers`, `bootstrap <-> subagent`. Breaking them needs type relocation.

---

**8. Remember before changing code**

1. **Do not break prefix stability.** Tool-definition order, message order and the system-prompt position decide the cache hit rate. Injected context is appended at the end of the request for this reason.
2. **`Option<u64>` in `Usage` is intentional.** "Not reported" is not zero.
3. **Compaction rebuilds `self.messages`.** Any "remember an index, then rebuild the container" pattern is fragile; clamp indices after compaction.
4. **Keep the SDK facade small.** Before adding to it, ask whether it is a fundamental or a product decision. Bots, memory, skills, todo and the pool are product decisions and live in the application.
5. **The boundary is the sandbox, not the command string.** `bash` has no blacklist by design.
6. **One writer per piece of state.** The pool file, a session log and a bot's `Agent` each have exactly one owner; do not open a second handle.
7. **Do not add a second name for a concept.** It is a bot, a conversation, a task, a memory. Do not reintroduce "project", "workspace" or "session list".

**Do not abstract for abstraction's sake.** Implement the one scenario, and keep only the capabilities that proved they will be needed again.
