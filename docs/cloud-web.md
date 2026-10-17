# Cloud web interface (only-chat)

> ⚠️ This document describes the **single-project transport implementation** that shipped first. The multi-project / lazy-loading / remote-desktop design supersedes the "one workspace" premise below — see [`cloud-server.md`](./cloud-server.md). Where the two disagree, `cloud-server.md` is the target.

## What it is

A **server-side** web version of Shirley: the Agent runs on the server (its tools —
`bash` / `read` / `grep` / `todo` / memory / skills / subagent — execute in the
**server's** environment), and the browser is the **desktop frontend**, unchanged.

> "only chat" describes the **UI shape**, not a reduced tool set. There is **no project
> or workspace management** — exactly one workspace (the server process's working
> directory) and a flat list of sessions. The Agent keeps its full tool set; the tools
> simply run on the server.

The guiding principle (from the user): *"it's essentially just the web version of the
desktop app; the only difference is that SDK data is processed on the server instead of
the client."* So this module does **not** ship a second UI — it serves the **same React
build** (`src/interface/desktop/web/dist`) and only swaps the **transport** in
`bridge.ts` from Tauri `invoke` / `listen` to HTTP + SSE.

## Why it is a sibling interface (not "a web layer on the TUI")

Same reasoning as `docs/desktop-interface.md`: the TUI is full of ratatui-specific logic
and cannot render a webview. The shared LCA is
`bootstrap::AgentFactory` + `interface::session::SessionManager` + the
`desktop::wire::AgentEventWire` protocol. The cloud server reuses all three and only
swaps the transport.

The current implementation is single-project: one `AgentFactory` + one `SessionManager`,
no `ProjectRegistry`. The target shape is described in
[`cloud-server.md`](./cloud-server.md), which reuses `ProjectRegistry` + lazy per-project
runtimes; this document's "one workspace" premise is not the end state.

### The implicit project (`__chat__`)

The desktop frontend still speaks a *project* contract (it was written for the
multi-project desktop app). To reuse it verbatim, the current implementation exposes
**one implicit project** whose id is `__chat__` (`server.rs::CHAT_PROJECT_ID`, identical
to the frontend's `CHAT_PROJECT_ID`). The frontend derives `mode === "chat"` from that
id, so it renders the plain chat surface automatically: no project sidebar,
`projectName="Chat"`, sessions folded into a single "Recent" list.

> **This `__chat__` shim is a stopgap, not the end state.** It exists only because the
> single-project transport shipped first; it is *not* a design conclusion that the server
> has no project layer. The target is the multi-project / lazy-runtime design in
> [`cloud-server.md`](./cloud-server.md), where real `ProjectRegistry` entries replace the
> single `__chat__` tag. Until then, the id is only a routing tag that makes the shared
> frontend switch into chat mode — there is no project layer behind it.
>
> It is also *not* the same as the desktop's `__chat__` pseudo-project
> (`shell.rs::CHAT_PROJECT_ID`), which still binds to a local directory and still walks
> the full multi-project command surface.

## Cargo feature

Gated behind the `server` feature so the default build does not pull in axum / tower-http:

```sh
cargo run --features server -- --server          # or SHIRLEY_INTERFACE=server
# listens on http://127.0.0.1:8787 by default; override with SHIRLEY_SERVER_ADDR
```

Without the feature, `--server` returns a clear "build with `--features server`" error.

## Code layout

| Path | Responsibility |
| --- | --- |
| `src/interface/server.rs` | axum router; `ServerState`; single `POST /api/cmd/{name}` dispatcher; per-turn driver (`spawn_turn`); SSE fan-out; static hosting of the desktop build. |
| `src/interface/desktop/web/` | the **unchanged** React frontend. Only `src/lib/bridge.ts` gained a third transport (`createHttpBridge`). |
| `src/interface/desktop/web/dist/` | the build output served by the server (`npm run build`). |
| `src/main.rs` | `Mode::Server` selection (`--server` / `SHIRLEY_INTERFACE=server`). |
| `src/interface/mod.rs` | `#[cfg(feature = "server")] pub mod server;` |

### Static hosting

The server serves `desktop/web/dist` at `/` via `tower_http::ServeDir`, with
`ServeFile(index.html)` as the SPA fallback (so deep links work). The dist directory is
resolved at startup: `SHIRLEY_WEB_DIST` if set, otherwise
`<CARGO_MANIFEST_DIR>/src/interface/desktop/web/dist`. It is read from disk at runtime —
rebuild the frontend (`cd src/interface/desktop/web && npm run build`) and reload the
page; no Rust rebuild needed.

## Transport contract

The frontend's `bridge.ts` is already dual-mode (`isTauri ? createTauriBridge() :
createMockBridge()`). The cloud build adds a third branch: in a non-Tauri production build
(`import.meta.env.DEV === false`) it uses **`createHttpBridge()`** — `npm run dev` still
uses the mock so the UI can be debugged standalone.

### Commands — `POST /api/cmd/{name}`

One endpoint per `AgentBridge` method. Body = the args object (JSON); response = JSON (or
a plain-text error with a non-2xx status). The `{name}` is the command name (`agent_send`,
`agent_list_sessions`, …). Keeping it a single dispatch means the client transport stays a
uniform `fetch` shim with no per-command routing table.

| Command | Args | Returns |
| --- | --- | --- |
| `agent_list_projects` | `{ archived? }` | `ProjectEntryWire[]` — always one implicit `__chat__` project |
| `agent_add_project` | — | `null` (single workspace: adding a project is meaningless) |
| `agent_rename_project` / `agent_archive_project` / `agent_restore_project` | — | no-op (204) |
| `agent_send` | `{ project, text, references?, images?, session? }` | 202 (events via SSE) |
| `agent_resend` | `{ project, text, references?, images?, session? }` | 202 — rewinds last turn then sends |
| `agent_cancel` | `{ project, session }` | 204 |
| `agent_subscribe` | `{ project, session }` | `SessionSnapshotWire { session, items, running, todo }` |
| `agent_unsubscribe` | `{ project, session }` | 204 (events are a global SSE stream; nothing to tear down) |
| `agent_model_name` | `{ project }` | `string \| null` |
| `agent_list_models` | `{ project }` | `{ label, value, provider }[]` |
| `agent_set_model` | `{ project, model }` | 204 (hot-swap, no Agent rebuild) |
| `agent_search_files` | `{ project, query }` | `{ path, name, kind }[]` (`@` references) |
| `agent_list_skills` | `{ project }` | `{ name, description }[]` |
| `agent_activate_skill` | `{ project, session, name }` | 204 |
| `agent_list_sessions` | `{ project }` | `SessionEntryWire[]` |
| `agent_current_session` | `{ project }` | `string \| null` |
| `agent_new_session` | `{ project, title? }` | `SessionEntryWire` |
| `agent_switch_session` | `{ project, name }` | 204 |
| `agent_rename_session` | `{ project, name, title }` | 204 |
| `agent_delete_session` | `{ project, name }` | 204 |
| `agent_move_session` | `{ project, name, targetProject }` | 400 (single workspace has nowhere to move) |
| `agent_load_history` | `{ project }` | `{ role, content }[]` (User / Assistant) |
| `agent_list_subagents` | `{ project, session }` | `SubagentSummaryWire[]` |
| `agent_load_subagent` | `{ project, session, id }` | `{ role, content }[]` |
| `agent_cancel_subagent` | `{ project, session, id }` | 204 |

The Rust-side argument struct is a single tolerant `CmdArgs` (`#[serde(default)]`, all
fields `Option`), so unknown / missing keys never fail decoding.

### Events — `GET /api/events` (SSE)

A single Server-Sent Events stream per browser tab (`text/event-stream`). The server
broadcasts every `AgentEventWire` (serialized JSON, tagged with `project="__chat__"` +
the session name via `with_route`) so the frontend routes events to the right session
view through `routeKey(project, session)`; each event is emitted as `event: agent_event`
with `data: <json>`. A `todo` tool completion additionally pushes a `TodoUpdated`
snapshot. This is the **same** event protocol the desktop app consumes
(`web/src/types/wire.ts`), so the two stay aligned.

SSE was chosen over WebSocket because this channel is **downstream-only** (streaming
deltas, long-running turns, background sessions) — there is no client→server traffic on
it, so SSE is lighter (no upgrade handshake), gives free reconnection via `EventSource`,
and traverses proxies / CDNs more reliably than a WS upgrade. Concretely: on the Rust
side an axum `Sse<impl Stream>` adapts a `broadcast::Receiver<String>` into the event
stream; in `bridge.ts`, `createHttpBridge` uses `new EventSource('/api/events')` instead
of `new WebSocket(...)`. Because `EventSource` cannot set headers, auth is passed as
`?token=`.

> This is the **current decision**; WS remains a viable alternative. If a future need
> arises for bidirectional traffic (cancel / ack / heartbeat) on the event channel,
> re-evaluate WS at that point. The command surface stays request-response on
> `POST /api/cmd/{name}` either way.

`openSession` ordering (from `App.tsx`): the frontend registers its per-session event
handler **first**, then awaits the `agent_subscribe` snapshot. Events that arrive before
the snapshot lands are buffered and replayed in order once it does — so the HTTP bridge
needs no dedup logic: `handlers.set` then POST the snapshot.

## Turn lifecycle (`spawn_turn`)

Mirrors the desktop `shell.rs::spawn_turn`:

1. `SessionManager::begin_turn` — takes the session's `Agent` out (rejects if running).
2. `memory.set_query` + `prefetch_query_vector` (best-effort, cancellable).
3. Register a `oneshot` cancel handle in `cancels`.
4. `tokio::spawn` a task that drives `agent.run_stream_content(...)`, applying each
   event into the `Session` **and** broadcasting it to SSE subscribers.
5. On terminal (Completed / Cancelled / error / panic via `catch_unwind`):
   `finish_interrupted_turn()` preserves partial text, `restore_agent` puts the Agent
   back, the terminal event is emitted, and `auto_title_session` runs (20 s timeout).

## Known gaps / boundaries

- **Single user, no auth.** `config_dir` is still the single-machine single-user
  assumption. Multi-user / per-user storage / authentication is **not** implemented.
- **Session storage** stays `FileSessionCatalog` (local JSONL). No database.
- The server does not reuse `ProjectRegistry`: there is exactly one workspace and one
  flat session list. This is a **current-implementation simplification**, not a design
  conclusion — the target is `ProjectRegistry` + lazy per-project runtimes in
  [`cloud-server.md`](./cloud-server.md).
- Tool-visible behavior is unchanged from the desktop app — the same tools, the same
  workspace constraints, just executed server-side.
