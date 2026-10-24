**Shirley technical design · Cloud server (multi-project HTTP + SSE)**

> Current connection/sync behavior: [Personal memory sync and remote chats](./memory-sync.md). Desktop uses a native authenticated HTTP/SSE proxy selected from the top-bar connection dialog. API authentication is implemented with `SHIRLEY_SERVER_TOKEN`; non-loopback listening requires a token. The older `serverUrl` selection and “no auth” descriptions below are historical. Project path allow-lists remain unimplemented.


This document describes the "cloud server" interface: running the whole Shirley Agent **on a
server** and reaching it from a browser, while the browser runs the **same desktop React build**
(`crates/shirley/src/interface/desktop/web/dist`). It is a **sibling** of `desktop-interface.md` /
`multi-session.md`. The desktop already had every shape the cloud needed (`ProjectRegistry` +
lazy per-project runtimes + per-project `SessionManager`), and the original `server.rs` was only a
**degenerate, single-project copy** of it; the core move was to lift the desktop orchestration
into one transport-agnostic core and make both `shell.rs` (Tauri) and `server.rs` (HTTP) thin
transport layers over it — which is now the landed state (see the status block below).

> **Status: landed (P0 + P1 + SSE).** The `ProjectRuntimeManager` core, the `index.json` session
> catalog, cloud project creation (server-side path), the `bridge.ts` fourth state, and the SSE
> event transport all exist in the code today. Landing map:
>
> | Piece | Where it lives |
> | --- | --- |
> | Transport-agnostic core `ProjectRuntimeManager` | `crates/shirley/src/interface/runtime/mod.rs` (~966 lines: registry + lazy `runtimes` + `cancels` + `EventSink`; all project / session / run / model / file / skill / subagent commands + wire DTOs) |
> | Thin Tauri transport | `crates/shirley/src/interface/desktop/shell.rs` (~474 lines: 27 `#[tauri::command]`s delegate to the core; per-session `publish` pump) |
> | Thin HTTP + SSE transport | `crates/shirley/src/interface/server/mod.rs` (~437 lines: `POST /api/cmd/{name}`, `GET /api/events` via `Sse` + `stream::unfold`, `ServeDir` SPA fallback; default `127.0.0.1:8787`) |
> | `index.json` session catalog | `crates/shirley/src/session.rs` (`FileSessionCatalog` lazy load: one read + temp+rename atomic write, mtime-hit reuse) |
> | `bridge.ts` fourth state | `isTauri && configuredServerUrl → createHttpBridge(serverUrl)`; `EventSource` listening on `agent_event` |
> | Convenience launchers | `.cargo/config.toml`: `cargo desktop` / `cargo web` / `cargo tui` |
>
> The sections below keep their original "current state vs target" framing as **design rationale**.
> Where they describe `server.rs` as a "partial copy" of the desktop, that describes the
> **pre-landing** state and has since been superseded by the shared core. Still open (**P2**):
> SSE auth middleware + `SHIRLEY_ALLOWED_ROOTS` allow-list, `agent_browse_dirs`, runtime LRU /
> idle unload, `load_tail(n)` paging — see §8–§9.

> **User requirement (verbatim, Chinese)**: 「云端 web 版直接复用 desktop 的 React 前端（设计风格完全对齐），不要另造一套 UI。」

---

**1. Where we started: the desktop already had the shape, the server was a partial copy**

> The table below records the **pre-landing** state (before the shared core existed); it is kept as
the rationale for *why* the extraction was done. The landed state is the shared `ProjectRuntimeManager`
described in §2.

Both interfaces already share the same LCA: `bootstrap::AgentFactory` + `interface::session::SessionManager`
+ the `AgentEventWire` contract (`crates/shirley/src/interface/desktop/wire.rs`). The difference is **how many
projects** each one knows about.

| Concern | Desktop (`shell.rs`, Tauri) | Server (`server.rs`, HTTP) |
| --- | --- | --- |
| Project model | Full `ProjectRegistry` (many projects) | Hard-coded single `__chat__` project |
| Project state | `DesktopState { registry, runtimes, pumps, cancels }` (`shell.rs:83`) | `ServerState { factory, sessions, cancels, events }` (`server.rs:82`) |
| Runtime per project | Lazy `ensure_runtime(project)` (`shell.rs:162`) | None — one eager `SessionManager` |
| Session orchestration | Per-project `SessionManager` (`ProjectRuntime`, `shell.rs:71`) | One global `Arc<Mutex<SessionManager>>` |
| Event transport | Tauri `app.emit` (`agent://event`) | One global WebSocket (`GET /ws`) |
| `list_projects` | Real registry list | Hard-coded `[{ id: "__chat__", name: "Chat", … }]` (`server.rs:448`) |

The server module's own header states the design intent honestly: *"the cloud web version should feel
like the desktop frontend, running on a server"*, and it deliberately does **not** reuse
`ProjectRegistry` because "only chat" = a UI shape, not a reduced tool set
(`server.rs:22-33`). That was a reasonable first cut. This document argues that the "only chat"
shape should be expressed as **a single project among many**, not as a separate code path — because
the frontend already supports multiple projects and the desktop already implements them.

Two concrete consequences of the current split:

- `server.rs::run()` does a **one-shot eager** resume at startup: `catalog.list()` → `open` →
  `load` → `build_agent`, then wraps it in one `SessionManager` (`server.rs:96-140`). There is no
  lazy per-project runtime; the server's working set is fixed at boot.
- `agent_add_project` in the dispatch table is a **stub returning `Null`** (`server.rs:400`), and
  `list_projects` ignores the registry entirely (`server.rs:448-457`). The frontend's project
  commands are accepted but do nothing meaningful.

---

**2. Target architecture: one shared `ProjectRuntimeManager`, two thin transports**

Lift the desktop's "registry + lazy runtimes + per-project `SessionManager`" into a
**transport-agnostic core** — call it `ProjectRuntimeManager` — and make `shell.rs` and `server.rs`
both just serialize commands into it and forward its events out.

```
                         ┌──────────────────────────────────────────┐
                         │        ProjectRuntimeManager (core)       │
                         │  registry: ProjectRegistry               │
                         │  runtimes: HashMap<project, Runtime>     │
                         │    Runtime { factory, sessions }         │
                         │  ensure_runtime / list_projects / ...    │
                         │  spawn_turn(project, session) -> events  │
                         └───────────────┬──────────────┬───────────┘
                                         │              │
                    ┌────────────────────┘              └────────────────────┐
                    │                                                        │
       ┌────────────▼─────────────┐                         ┌────────────────▼──────────────┐
       │ shell.rs (Tauri)         │                         │ server.rs (HTTP)               │
       │  invoke -> core method   │                         │  POST /api/cmd/{name} -> core  │
       │  core event -> app.emit  │                         │  core event -> SSE frame       │
       │  (agent://event)         │                         │  (GET /api/events)             │
       └────────────┬─────────────┘                         └────────────────┬───────────────┘
                    │                                                        │
                    └───────────────┬────────────────────────────────────────┘
                                    │
                    ┌───────────────▼────────────────┐
                    │  bridge.ts (React frontend)     │
                    │  Tauri bridge | HTTP bridge      │
                    │  same AgentBridge contract       │
                    └─────────────────────────────────┘
```

The key property: **the core never owns a `broadcast` channel or a socket**. It takes an
`Fn(AgentEventWire)` sink (or returns a `Stream`) and lets the transport decide where the bytes go.
Tauri turns the sink into `app.emit`; the server turns it into an SSE frame. Because both transports
run the *same* core, the command semantics are identical by construction — which is exactly why
"desktop connecting to a remote server" (§6) needs **zero new protocol**.

### 2.1 Proposed interface (signature sketch)

```rust
// Transport-agnostic core. `Arc<Mutex<..>>` / `spawn_blocking` details omitted.
pub struct ProjectRuntimeManager { /* registry, runtimes, ... */ }

impl ProjectRuntimeManager {
    // -- projects ------------------------------------------------------------
    pub async fn ensure_runtime(&self, project: &str) -> Result<Arc<Mutex<ProjectRuntime>>, CoreError>;
    pub fn list_projects(&self, include_archived: bool) -> Vec<ProjectEntry>;
    pub async fn add_project(&self, path: PathBuf, name: Option<String>) -> Result<ProjectEntry, CoreError>;
    pub async fn rename_project(&self, project: &str, name: &str) -> Result<(), CoreError>;
    pub async fn archive_project(&self, project: &str) -> Result<(), CoreError>;
    pub async fn restore_project(&self, project: &str) -> Result<(), CoreError>;

    // -- sessions ------------------------------------------------------------
    pub async fn list_sessions(&self, project: &str) -> Result<Vec<SessionEntry>, CoreError>;
    pub async fn current_session(&self, project: &str) -> Result<Option<String>, CoreError>;
    pub async fn new_session(&self, project: &str, title: Option<&str>) -> Result<SessionEntry, CoreError>;
    pub async fn switch_session(&self, project: &str, name: &str) -> Result<(), CoreError>;
    pub async fn rename_session(&self, project: &str, name: &str, title: &str) -> Result<(), CoreError>;
    pub async fn delete_session(&self, project: &str, name: &str) -> Result<(), CoreError>;

    // -- turns / events ------------------------------------------------------
    /// Start one turn; every `AgentEventWire` goes to `sink`. Returns a cancel handle.
    pub async fn spawn_turn(
        &self,
        project: &str,
        session: &str,
        input: TurnInput,
        sink: Box<dyn Fn(AgentEventWire) + Send + Sync>,
    ) -> Result<CancelHandle, CoreError>;
}
```

### 2.2 Event outbound abstraction

The core produces `AgentEventWire` (already the shared contract, see `wire.rs:47-70` and
`web/src/types/wire.ts`). The transport adapter is responsible only for the wire:

| Transport | Command in | Event out |
| --- | --- | --- |
| Tauri (`shell.rs`) | `invoke("agent_send", …)` | `app.emit("agent://event", wire)` |
| HTTP (`server.rs`) | `POST /api/cmd/agent_send` | SSE frame on `GET /api/events` |

Because `AgentEventWire` is transport-agnostic JSON, neither side invents a new type. The
`project` / `session` fields already tag every event so the frontend can route it
(`wire.rs:43-46`), and the per-session fan-out already exists on the desktop side.

---

**3. ① Lazy session loading**

The repository has already made two of the four layers lazy; the remaining two are the real cost.

| Layer | Status today | Evidence |
| --- | --- | --- |
| Project runtime | ✅ lazy (`ensure_runtime` builds on first use) | `shell.rs:162` |
| Session `Agent` | ✅ lazy (only the active session's `Agent` exists; `switch_to` loads on demand) | `SessionManager` / `docs/multi-session.md` |
| **Session list metadata** | ❌ **eager** — `list()` scans every `.jsonl` and reads each file fully | `session.rs:422-461`, `summary_of` at `session.rs:557` |
| **Session content first load** | ⚠️ **eager at runtime build** — `runtime_from_factory` loads the latest log immediately | `shell.rs:112-150` |

### 3.1 The `list()` cost

`FileSessionCatalog::list()` (`session.rs:422`) walks the directory, and for **every** `.jsonl` file
calls `summary_of(&path)` (`session.rs:446`), which opens the file and reads it **line by line to the
end** to count user turns and extract the first user-message preview (`session.rs:557-577`). So the
cost is **O(number of sessions × full file length)** on every `agent_list_sessions`. The sidebar
polls this; with many long sessions it becomes the dominant latency.

### 3.2 Proposal: one `index.json` per workspace

Replace the per-file scan with **one metadata index per workspace directory**, written incrementally
and read in a single pass.

```
~/.shirley/workspaces/<workspace-id>/sessions/
├── index.json          <- NEW: session metadata index (one read)
├── 2024-06-01-a.jsonl  <- full raw Message log (source of truth)
├── 2024-06-01-a.title  <- title sidecar (unchanged)
└── 2024-06-02-b.jsonl
```

```jsonc
// index.json schema
{
  "version": 1,
  "sessions": [
    {
      "name": "2024-06-01-a",
      "label": "Refactor the adapter layer",
      "preview": "Can you migrate the TS backend to Rust…",
      "modified_ms": 1717200000000,
      "turns": 12
    }
  ]
}
```

**Write timing** — the index is updated incrementally, never rebuilt from a full scan in steady state:

| Event | Index update |
| --- | --- |
| `append` (end of a turn) | Upsert that session's entry: `modified_ms` = now, `turns` += 1, `preview` filled on first user message |
| `rename` | Update that entry's `label` (title sidecar still written, `session.rs:528-551`) |
| `delete` | Remove that entry |
| `create` / `create_lazy` | Insert an entry (lazy: insert only when it materializes on first append) |

**Consistency fallback** — on `list()`, `read_dir` once and compare the **set of file names** against
the index. Any `.jsonl` missing from the index is backfilled with one `summary_of()` call and written
back; any index entry whose file is gone is dropped. In steady state this is **zero full-file reads**:
only the name set is compared, and only new/orphan files pay the `summary_of` cost once.

### 3.3 Why one `index.json`, not a `<name>.meta.json` sidecar

| Option | Reads per `list()` | Atomicity | Notes |
| --- | --- | --- | --- |
| **`index.json` (chosen)** | 1 | `temp` + `rename` (same trick as `ProjectRegistry::save`, `project.rs:127-129`) | Whole list in one read; one atomic replace |
| `<name>.meta.json` sidecar | N | per-file temp+rename | N stat+read, and the directory already carries a `.title` sidecar — a second sidecar multiplies file count |

The single-file index matches the existing atomic-write idiom already used by the project registry,
and it collapses the common "show the sidebar" path to a single file read.

### 3.4 Two-phase first runtime

`runtime_from_factory` (`shell.rs:112`) currently does `list()` → `open(latest)` → `load()` →
`build_agent()` on the **first** lazy load of a project, so opening a project with a big latest
session pays the full-log load up front. The target splits this into two phases:

1. **`ensure_runtime`** builds only the `SessionManager` + an **empty** `Agent` (`create_lazy`) —
   naming the session without loading history. The sidebar renders immediately.
2. **`switch_session` / `agent_load_history`** performs `store.load()` + `build_agent()` only when
   the user actually opens a session.

`P2` optional: `load_tail(n)` paging — load only the last `n` messages into the `Agent` working set
and page earlier history in on demand, so a 10k-message session does not build a 10k-message agent
just to show the last screen.

---

**4. ② Cloud project creation**

The hard part is that a **browser cannot use a native folder picker to choose a directory on the
server**. The desktop uses `tauri-plugin-dialog`; the browser has no such thing.

### 4.1 Option A (recommended, do first): server-side path text box

Add `agent_add_project { path, name? }` → `registry.add(canonicalize(path))` on the server
(`project.rs:156`). The frontend, when in **server mode**, swaps the Tauri folder picker for a plain
**path text input**. The bridge exposes a capability flag so the UI can branch:

```ts
// bridge capability surface (new)
interface BridgeCapabilities {
  nativeFolderPicker: boolean; // true for Tauri, false for HTTP
}
```

No change to the multi-project UI itself: once `list_projects` returns real entries, the frontend
automatically leaves "chat mode" and shows the project sidebar (it already keys `mode === "chat"`
off `id === CHAT_PROJECT_ID`).

### 4.2 Option B (P2): server-side directory browser

`agent_browse_dirs { path }` — a server endpoint that lists subdirectories of a given path, letting
the frontend build a folder browser in the web UI. More UX work, no security gain over A (both need
the same path allow-list, §7), so it is deferred.

Both options write to the **same `projects.json`**, project id stays the **canonical path**, and the
frontend multi-project UI needs **zero changes** — returning real projects is enough to switch it
from chat mode to project mode.

---

**5. Transport: SSE (decided)**

Events must be **server-pushed** and are independent of any single request (streaming deltas, long
tasks, background sessions), so they cannot ride on the request/response command channel. The chosen
event transport is **SSE**.

### 5.1 Decision

| Face | Endpoint | Protocol |
| --- | --- | --- |
| Commands | `POST /api/cmd/{name}` | JSON in / JSON out (already implemented in `bridge.ts:544`) |
| Events | `GET /api/events` | `text/event-stream` (SSE) |

### 5.2 Why SSE over WebSocket

- **Events are one-directional.** The current WS client→server direction is already dead air:
  `handle_socket` ignores incoming frames (`Some(Ok(_)) => {}`). SSE's one-way nature matches the
  actual traffic exactly.
- **Lighter.** No upgrade handshake, `EventSource` reconnects automatically, and it traverses
  proxies/CDNs more reliably than an upgraded socket.
- **Cost is a non-issue.** The only thing SSE cannot do is client→server push, which we do not need
  (cancel/ack, if ever needed, go through the command channel).

### 5.3 Migration

| From | To |
| --- | --- |
| `GET /ws` (WebSocket, `server.rs:39`) | `GET /api/events` (SSE) |
| `bridge.ts::createHttpBridge` uses `new WebSocket(...)` (`bridge.ts:566`) | `new EventSource('/api/events')` |
| `broadcast::Receiver<String>` → WS text frames | `axum::Sse<impl Stream>`; each `broadcast::Receiver<String>` item → `event: agent_event\ndata: <json>\n\n` |
| Auth (none today) | `?token=` query param (EventSource cannot set headers) |

Reconnect is free: `EventSource` retries on drop, so a page left open keeps receiving
background-session events without hand-written reconnect logic (contrast the current manual
`setTimeout(connect, 1000)` in `bridge.ts:578-581`).

> **Explicitly a current decision, not a law.** WebSocket remains a viable substitute. If a future
> requirement needs genuine bidirectional traffic (cancel / ack / heartbeat), re-evaluate WS then.
> For today's one-way event fan-out, SSE is the simpler correct choice.

---

**6. ③ Desktop connecting to a cloud server**

The desktop build should be able to point at a **remote** server instead of its in-process Tauri
backend, reusing the same HTTP bridge the browser uses.

### 6.1 Fourth state in `bridge.ts`

Today `agentBridge()` has three states (`bridge.ts:682-693`):

| State | Condition | Bridge |
| --- | --- | --- |
| 1 | `isTauri` (`bridge.ts:192`) | `createTauriBridge()` |
| 2 | dev, not Tauri | `createMockBridge()` |
| 3 | prod, not Tauri | `createHttpBridge()` |

Add a **fourth**:

```ts
cached ??= isTauri
  ? (configuredServerUrl
      ? createHttpBridge(configuredServerUrl)   // NEW: Tauri talking to a remote server
      : createTauriBridge())
  : dev
    ? Promise.resolve(createMockBridge())
    : Promise.resolve(createHttpBridge());
```

### 6.2 Why zero new protocol

Both ends run the **same `ProjectRuntimeManager`** (§2), so command names, argument shapes, and
event JSON are identical. `createHttpBridge` already implements the full command POST + event
subscription surface (`bridge.ts:555-678`), so pointing it at a remote base URL is enough.

### 6.3 Configuration source

- A **Tauri settings page** field for `serverUrl`, or
- a launch argument `SHIRLEY_REMOTE_SERVER=<url>`.

When set, the desktop behaves exactly like a browser pointed at that server: all Agent execution,
tool calls, and files live on the server.

---

**6.4 Storage root (shared, no per-interface split)**

There is **no** separate storage root per interface: desktop / TUI / cloud server all resolve
`storage::global_root()` to the same `~/.shirley` directory. Running them on the same machine
under the same OS user intentionally means they see the same `projects.json`, sessions, memory
and `config.toml`. The earlier idea of an isolated cloud root (`home_cloud_root()` /
`set_root_override()` / a process-level `OnceLock` override) was **dropped** — it does not exist
in the code, and keeping the cloud on its own root only ever produced "the web can't see my
desktop data" confusion. If a deployment genuinely needs isolation, run it as a different OS
user / container so the whole `~/.shirley` is separate; the app itself keeps one root.

`storage::global_root()` remains the single derivation point for `projects.json`,
`workspaces/<id>/sessions`, and `config.toml`. Legacy state that used to live under
`<config_dir>/shirley` is migrated into `~/.shirley` once by `storage::migrate_legacy_root()`
at startup.

---

**7. Deployment and security**

The current server is **127.0.0.1, single-user, self-hosted, no auth**. That is fine for local use
and unsafe the moment it is exposed.

| Scenario | Requirement |
| --- | --- |
| `127.0.0.1` loopback, single user | No auth acceptable (status quo) |
| `0.0.0.0` exposed | **Must** add auth: Bearer token on `POST /api/cmd/*`; `?token=` on the SSE stream (EventSource cannot set headers) |
| Either | **Project path allow-list** `SHIRLEY_ALLOWED_ROOTS` — `agent_add_project` / `agent_browse_dirs` must reject paths outside the allow-list |

The auth check is left as a **middleware mount point in `server.rs`** (an axum layer in front of the
router), to be filled in at **P2**. The path allow-list is the same shape: a check inside
`add_project` / `browse_dirs` before `canonicalize`.

### 7.1 One-command deploy: `scripts/deploy-cloud.sh`

The build → upload → switch → restart → verify loop is captured in **`scripts/deploy-cloud.sh`**.
Read the header comment first: it explains *why* each step is shaped the way it is (static musl, UTC
timestamped releases, atomic symlink swap, why `/api/status` must not be used for the health check).

```sh
# full deploy: npm run build + cargo zigbuild + upload + atomic switch + restart + health check
scripts/deploy-cloud.sh --host root@your-server

# other commands
scripts/deploy-cloud.sh build                  # build only (dist + static binary), no server access
scripts/deploy-cloud.sh releases --host root@your-server   # list releases, mark the active one
scripts/deploy-cloud.sh rollback --host root@your-server   # flip `current` back one release + restart
scripts/deploy-cloud.sh rollback --host root@your-server --release 20261020T152821Z

# useful flags
--host root@your-server --port 22  # a different machine
--skip-web                         # reuse the existing dist (backend-only change)
--skip-build                       # reuse target/<triple>/release/Shirley
--dry-run                          # print every remote action, mutate nothing
--no-rollback                      # do not auto-revert when the health check fails
--keep 5                           # after success, keep only the 5 newest releases
```

`--host` (or `SHIRLEY_DEPLOY_HOST`) is **required** for every command that touches the server
(`deploy` / `rollback` / `releases`); `build` needs no host. Everything else is overridable by
environment variable (`_PORT` / `_ROOT` / `_USER` / `_SERVICE` / `_HEALTH` / `_TOKEN`), so the same
script points at another host without editing it.

**Prerequisites (on the machine you build from):**

- `rustup target add x86_64-unknown-linux-musl`
- `cargo install cargo-zigbuild` + a `zig` on `PATH` (`brew install zig`)
- `npm install` once under `crates/shirley/src/interface/desktop/web`
- SSH key auth to the target host (`BatchMode=yes` is set, so an interactive password prompt will
  just fail — set up a key)

**What the script assumes on the target host** (all already true for the existing CentOS box):

| Assumption | Default | Override |
| --- | --- | --- |
| release root | `/opt/shirley` | `SHIRLEY_DEPLOY_ROOT` |
| `releases/<UTC ts>/` + `current` symlink | created by the script | — |
| release tree owner | `shirley:shirley`, binary `755` | `SHIRLEY_DEPLOY_USER` |
| systemd unit | `shirley.service` | `SHIRLEY_DEPLOY_SERVICE` |
| served dist | `<root>/current/web` (`SHIRLEY_WEB_DIST` in the unit) | — |
| health URL (from the server) | derived from the unit's `SHIRLEY_SERVER_ADDR` | `SHIRLEY_DEPLOY_HEALTH` |
| API token | read from `<root>/cloud.env` if present | `SHIRLEY_DEPLOY_TOKEN` |

**Upload transport.** The binary goes up with a single `scp`; the web dist (~450 small files) goes
up as **one streamed `tar`** over a single ssh connection (`tar -cf - . | ssh ... tar -xf -`). Copying
the dist file-by-file with `scp -r` is slow and, on a flaky link, gets cut mid-transfer
(`Connection closed by remote host`); a streamed tar is one connection, much faster, and is retried
as a whole. Every upload is wrapped in `retry 3`, so a transient drop does not abort the deploy.

**Health URL derivation.** Rather than hard-code a probe address, the script reads the unit's
`SHIRLEY_SERVER_ADDR` (`systemctl show -p Environment shirley`) and probes that host:port — a
wildcard bind (`0.0.0.0`) is rewritten to `127.0.0.1`. This matters because a *wrong* probe URL does
not just warn: the health check fails and the deploy auto-rolls back. `SHIRLEY_DEPLOY_HEALTH`
overrides the derivation when the service is fronted differently (e.g. behind a reverse proxy).

**Why the artifact must be static.** The target hosts are CentOS 7 (glibc 2.17). A glibc-linked
build either refuses to start or needs a newer libc than the host ships. `cargo zigbuild --target
x86_64-unknown-linux-musl` produces a statically linked, stripped ELF; the script runs `file` on the
result and **aborts if it is not `statically linked`**, so a misconfigured build never reaches the
server.

**Health check (bounded, no hangs).** After the restart the script verifies, in order: the unit is
`active`; `GET /` returns 200; every `assets/...` file referenced by `index.html` exists on disk
(catches a stale `SHIRLEY_WEB_DIST`); an **unauthenticated** `POST /api/cmd/...` returns 401/403
(auth still enforced — a 200 here means the server is exposed); and, when a token is available, an
authenticated `POST` returns 200. Every remote `curl`/`ssh` carries a timeout. **Do not add an
`/api/status` probe** — it is an SSE stream, its body stays empty without pushes, and a plain `curl`
will hang until it is killed (this happened once).

**Failure handling.** If the unit does not become active or the health check fails after the switch,
the script re-points `current` at the previous release and restarts (unless `--no-rollback`), so a
bad build does not leave production down.

**Note on the PWA.** The dist ships a service worker (`sw.js`). After a deploy, a browser may keep
serving cached old assets; ask the user to hard-reload (Cmd/Ctrl+Shift+R) or unregister the service
worker in DevTools to confirm the new build.

---

**8. Landing order**

| Phase | Work | Outcome |
| --- | --- | --- |
| **P0** | Extract `ProjectRuntimeManager` from `shell.rs`; make `shell.rs` a thin Tauri transport over it; make `server.rs` a thin HTTP transport over it (same core). | One core, two transports; server gains multi-project for free. |
| **P0** | `index.json` session catalog (incremental write + name-set fallback). | `list_sessions` stops doing O(sessions × file) work. |
| **P1** | `agent_add_project { path, name? }` (server path text box) + `bridge.ts` `nativeFolderPicker` capability branch. | Cloud users can add real projects; sidebar switches out of chat mode. |
| **P1** | `bridge.ts` fourth state: `isTauri && serverUrl → createHttpBridge(serverUrl)`. | Desktop can connect to a remote server, zero new protocol. |
| **P2** | SSE migration (`GET /ws` → `GET /api/events`, `EventSource`). | Lighter, auto-reconnecting event transport. |
| **P2** | `agent_browse_dirs` (server directory browser) + `load_tail(n)` paging. | Nicer cloud project creation; large-session performance. |
| **P2** | Auth middleware + `SHIRLEY_ALLOWED_ROOTS` allow-list. | Safe to expose beyond loopback. |

> Note: the SSE migration is listed at P2 for sequencing, but it is a **precondition** for a
> production cloud deployment — it can be pulled earlier if the server is exposed before P2.

---

**9. Open decisions**

These remain **undecided** and are called out so the next agent does not silently pick one:

1. **Path text box vs directory browser** for cloud project creation — Option A (path input) is
   recommended first; Option B (browse endpoint) is a P2 UX improvement. Not settled.
2. **`127.0.0.1` no-auth vs `0.0.0.0` + auth** — depends on the deployment target; both must exist,
   but the default exposure posture is not fixed.
3. **`index.json` vs `<name>.meta.json` sidecar** — `index.json` is recommended (one read, one atomic
   write); not formally ratified.
4. **Desktop → cloud: fourth bridge state only, vs also embedding a server inside Tauri** — this
   document assumes the fourth state only; embedding a local server in the desktop is a heavier
   alternative not chosen here.
5. **Runtime LRU / idle unload** — should `runtimes` evict idle project runtimes to bound memory?
   Today the map grows with opened projects; an LRU/idle-unload policy is an open question.

---

**10. Summary**

The desktop already contained everything the cloud server needs: a `ProjectRegistry`
(`project.rs`), lazy per-project runtimes, per-project `SessionManager`, and a transport-agnostic
`AgentEventWire` contract (`wire.rs`). That extraction is now **done**: `ProjectRuntimeManager`
(`runtime.rs`) is the single core, and `shell.rs` (Tauri `emit`) and `server.rs` (HTTP + SSE) are
thin transports over it, differing only in transport. Multi-project cloud, cloud project creation,
and desktop-to-remote-server all fall out of the same machinery, and the frontend `AgentBridge`
contract is untouched. What remains is P2 hardening (auth, path allow-list, runtime eviction,
paging) — see §8–§9.
