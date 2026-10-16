**Shirley technical design · Multi-session parallelism**

This document continues from `session.md`. Section 8 of `session.md` solves "**owning and switching**
multiple sessions" -- at any moment only one session runs, and switching moves the `Agent` from the old
session to the new one (serially). This document advances that to "**multiple sessions running at the
same time**": multiple `Agent`s each `run_stream`, without blocking each other, with only one shown in
the foreground and the background ones continuing to run.

**Scope statement**: this round only fixes the architecture and seams of "multi-session parallelism";
it does not change the semantics of `Message` / `AgentEvent` (appending event **fields** is allowed,
variant semantics are unchanged). The persistence layer only needs a new lock when "the same session is
written by multiple drivers at the same time"; **parallelism across different sessions needs no change
to persistence**.

---

**1. First split "multiple sessions running at the same time" into three tiers (setting the tone
first)**

The phrase is ambiguous, and different interpretations differ by an order of magnitude in change
volume. Fix the boundary first, to avoid mixing three things into one:

| Tier | Meaning | In scope of this document |
| --- | --- | --- |
| **A. Multiple sessions running in parallel** | Multiple `Agent`s each `run_stream` at the same moment, without blocking each other | **the main body of this document** |
| **B. Multiple drivers on the same session** | The same session log read and written by multiple drivers at once | this document gives the boundary, **does not implement** |
| **C. Background suspension + foreground switching** | Only the foreground session runs; the others are suspended | the status quo is close; this document absorbs it along the way |

**The current state is a prototype of C**: `/session` can switch, but only one `Agent` can run at a
time (the TUI uses `take_agent` to move the `Agent` out, and a session operation during switching
directly reports "agent is running"). This document develops **A**.

> **Landing status (current code in this repository)**: **the P1 + P2 event-channel part is landed**
> (decisions 4 / 5 / 9, and decision 2's "one channel per session").
> The application layer `src/interface/session.rs` already has `SessionManager` + `Session` (each
> session holds its own independent `Agent`); `src/bootstrap.rs` has changed from `Bootstrap`
> (producing a single `Agent`) to `AgentFactory` (a `build_agent(messages)` factory); on the SDK side
> `Agent::switch_session` has been removed, and both the TUI and desktop now switch through the
> `SessionManager.active` pointer.
> **One channel per session is landed**: `Session` holds its own
> `events: broadcast::Sender<Result<AgentEvent, String>>` (decision 2 route A, using `broadcast`
> rather than `mpsc` -- desktop allows "reconnect subscription", and `broadcast` naturally supports
> multiple subscribers + realigning by resending a snapshot after `Lagged`); the accumulation logic
> converges into `Session::apply_event`, and **the TUI and desktop share the same event-handling
> path**; `SessionManager` provides `begin_turn` / `restore_agent` / `apply_event` / `subscribe` /
> `snapshot` by session name, the desktop's `agent_send(session)` drives by name, and
> `agent_subscribe` does `subscribe` + `snapshot` under the same lock (no loss, no duplication).
> **Not yet landed**: decision 3's SDK cancellation seam, decisions 7/8's maximum parallelism limit
> and queueing, and decision 6's tier-B persistence lock.

**Decision 1: multi-session = multiple independent `Agent`s, not "one Agent time-sharing".**

The reason is already hard-wired into the SDK's types:

- `Agent::run_stream(&'a mut self, ...) -> Stream + Send + 'a` **borrows** `self`, so a single `Agent`
  cannot be driven concurrently at the type level (enforced by the borrow checker).
- `todo` does `Arc::new(...)` inside `Agent::new`, **one per `Agent`** -- when multiple `Agent`s run in
  parallel, the task ledgers are naturally isolated and do not cross sessions.
- There is no global mutable state in the whole repository (`static` / `OnceLock` / `thread_local`
  only hit test locks).

So the correct form of "multi-session parallelism" is **one `Agent` per session**, not cloning or
time-sharing. This also means `Agent::switch_session` (that serial-switching seam on the SDK side)
**should not exist**: "which session is active" is application-layer orchestration, and the SDK should
not have the concept of a "current session" (see decision 9 in section 7).

---

**2. SDK status: basically ready, missing only two things**

The conclusion after an on-the-ground check: **the SDK layer hardly needs to change**.

**Already ready**:

- `Agent` can be constructed independently, has no global state, and is `Send`;
- `todo` is isolated per instance (`Arc::new` inside `Agent::new`), no crossing under parallelism;
- `trait SessionStore: Send + Sync` (**now owned by the application layer `src/session.rs`**),
  `JsonlSessionStore` internally `Mutex<File>`, so a single instance's `append` / `load` / `truncate`
  are mutually exclusive;
- `ToolManager` / `ToolContext` are constructed with the `Agent`; `ToolContext` is
  `Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>`, and `Tool: Send + Sync`.

**The only two gaps**:

| Gap | Location | Impact |
| --- | --- | --- |
| **Events have no source identifier** | no variant of `AgentEvent` in `runtime/event.rs` carries a session / run id | under multi-session parallelism, the consumer cannot tell "which session this `ContentDelta` belongs to" |
| **No cancellation seam** | `run_stream` has no abort; `StopReason::Cancelled` is defined but never produced | cannot cancel a single session; the current "interrupt" relies on the caller dropping the stream, and can only act on "the one currently running" |

**Decision 2: the event source identifier is solved by "one channel per session" -- the channel is
held by `Session` itself, without changing the `AgentEvent` variants.**

Two routes:

- **Route A (adopted)**: each `Session` holds its own independent `mpsc` channel, so `run_stream`'s
  events are naturally isolated by channel; the consumer (the TUI / desktop main loop) receives events
  from each `Session`'s `rx`, with no need to stuff a source identifier into the events.
- **Route B (discarded)**: pass a `run_id` to `run_stream`, and have each event carry it.

Why choose A: `AgentEvent` is an external contract (`docs/README.md` principle 4: keep the contract
small), and a source identifier is **the caller's orchestration information**, not a fundamental SDK
capability; channel isolation also conveniently keeps the scheduling right of "who is running" in the
application layer. Route B is needed only when **the same `Agent` must be consumed by multiple
paths**, and decision 1 already rules out that form.

**Channel ownership (formerly open question 1, now decided): `Session` holds its own `tx`, and
`SessionManager` does not hold a channel.** Reason: symmetric with "each `Session` holds its own
`Agent`" -- the event channel is the session's private pipe and should not leak to the orchestration
layer; `SessionManager` only stores the `JoinHandle` (for recycling / cancellation). The demand of
"receive all sessions' events at once with one `select!`" is satisfied by the main loop polling each
`Session.rx` (or `FuturesUnordered`), without handing the channel upward. **Route B is not landed.**

**Decision 3: make the cancellation seam "an optional cancellation signal for `run_stream`".**

The existing `StopReason::Cancelled` is dead code, and multi-session parallelism will almost certainly
need it (a background session must be able to be stopped individually). Minimal intrusion: add an
overload / new method to `run_stream` that accepts a `tokio_util::sync::CancellationToken` (or
`oneshot::Receiver<()>`), responds to cancellation in the stream's `select!`, `drop`s the internal
request, and ends this round with `Cancelled`.

- This is **the same technique** as the TUI's current state (`select! { _ = &mut cancel => break }` +
  `drop(stream)` in `run_agent`), only promoted from "manual wrapping in the application layer" to "a
  formal SDK seam", so that it becomes addressable.
- No new variant is introduced: after cancellation, `Finished(RunResult { stop_reason: Cancelled, .. })`,
  which existing consumers can already handle (`StopReason` has had this value for a while).

> If only an MVP is wanted at first, the SDK can be left untouched: the application layer continues to
> cancel with "drop the stream", at the cost of **being unable to precisely address a particular
> background session**. The cancellation seam is added as a second step.

---

**3. Application-layer status: the singleton state model is the real blocker**

Both interfaces hard-wire "a single `Agent` + a single copy of UI state":

**TUI** (`App` in `src/interface/app.rs`):

```
agent: Option<Agent>              // a single Agent
items: Vec<Item>                  // a single message list
scroll / auto_scroll / max_scroll // a single scroll state
last_usage / total_usage          // a single usage statistic
context_usage: Option<(u64,u64)>  // a single context occupancy
current_session: Option<String>   // a single current session name
ui_turn_start / turn_prompt       // a single "this round" state
```

The `tui.rs` main loop uses the **serial** technique of `take_agent()` -> `tokio::spawn(run_agent(...))`
-> `restore_agent()`, and the `run_agent` result is sent back via a **single** `mpsc`.

**Desktop** (`DesktopState` in `src/interface/desktop/shell.rs`):

```
agent: Arc<Mutex<Option<Agent>>>
current_session: Arc<Mutex<Option<String>>>
```

In `agent_send`, `slot.lock().await.take().ok_or("agent is running")?` -- **a second concurrent call is
rejected outright**. Events go through a **single global** `EVENT_NAME = "agent://event"`, and the
frontend cannot tell the source.

**Decision 4: the application layer introduces `SessionManager` (globally unique) + `Session` (one per
session), with each `Session` holding its own `Agent`.**

`SessionManager` is the **single orchestration entry** newly added at the application layer -- it
collects three things in one place: "where sessions come from" (`SessionCatalog`), "how far each
session has gotten" (`Session`), and "who is in the foreground" (`active`):

```
SessionManager                          // application layer, globally unique
├── sessions: HashMap<SessionId, Session>
├── active: SessionId                   // who is in the foreground (a pure UI pointer, does not rebuild the Agent)
├── catalog: Arc<dyn SessionCatalog>    // where sessions come from (already exists)
├── build_agent: fn(Vec<Message>) -> Agent   // factory (decision 5; load first, then build the Agent)
└── max_parallel / pending queue         // scheduling (decisions 7 / 8)

Session                                 // one session = one runtime container
├── id: SessionId                       // stable identifier (reusing SessionEntry.name)
├── agent: Agent                        // **exclusive to this session**; other sessions cannot touch it
├── items: Vec<Item>                    // this session's message entries
├── usage / context_usage               // this session's usage
├── scroll / ui_turn_start / turn_prompt
├── state: RuntimeState                 // Idle | Running
└── events: broadcast::Sender<...>      // event send-back channel (decision 2 route A, actually broadcast)
```

**Why identity lives outside `Session` while `Agent` stays anonymous**: if `Agent` also held an id,
there would be two sources of truth, "one in `Agent` and one in `Session`". The factory `build_agent`
produces an **anonymous** `Agent`, and attaches an `id` once when stuffing it into `Session` --
identity is on the container, not inside it.

`App`'s responsibility moves up from "a single copy of state" to **`SessionManager`** (`App` degrades
to "the view of the current `Session`"); all existing methods (`append_streaming_delta` /
`record_usage` / `scroll_lines` / `take_agent` ...) either take a `SessionId` parameter or become "act
on the `manager.active` session".

> **Landing approach (deviations from the original design, all implementation details)**:
> - `App` keeps a `sessions: SessionManager` field and `impl Deref/DerefMut<Target = Session>`,
>   transparently resolving `self.items` / `self.agent` / `self.waiting` ... to `sessions.active()` --
>   so the existing dozens of methods need **zero changes**, and the semantics is exactly "act on the
>   `manager.active` session". The truly per-session fields (including `name`) live on `Session`.
> - **Channel ownership is landed per decision 2**: `Session` holds its own
>   `events: broadcast::Sender<...>`, and `SessionManager` does not hold a channel. The channel element
>   is `Result<AgentEvent, String>` -- `AgentError` is also sent out as an event (`Err`), and the
>   accumulator (`apply_event`) sets the error accordingly. Capacity
>   `EVENT_CHANNEL_CAPACITY = 256`; a lagging subscriber gets `Lagged`, and the subscriber (the desktop
>   pump) skips and realigns by resending a snapshot.
> - `App`'s accumulation methods (`append_streaming_delta` / `record_usage` / `start_compression` ...)
>   have **moved down into `Session`**, and `App` keeps only a thin wrapper (`App::apply_event`
>   forwards to `sessions.active_mut().apply_event` and invalidates `message_cache`). `message_cache`
>   is a pure render cache and stays in `App` (does not go into `Session`).
> - **The `Agent`'s take / restore is now by session name** (`begin_turn(name, user_message)` /
>   `restore_agent(name, agent)`): the desktop's `agent_send(session)` takes by name, fixing the
>   cross-session bug where "while a background session runs, the foreground has switched away, and
>   events / the Agent are misrecorded into another session".
> - **`begin_turn` also optimistically records the user message** (= take the `Agent` +
>   `push_message(Role::User)`, atomic): `Session::apply_event` **deliberately ignores**
>   `MessageAdded(User)` -- the user message is recorded by the **driver side** (the TUI records it in
>   `App::submit`, the desktop in `agent_send`), not by the event stream. Previously the desktop
>   missed it, so `Session.items` never contained the user message, and it "disappeared" when
>   switching away and back and rebuilding from a snapshot; the regression test
>   `session::tests::begin_turn_records_user_message_into_snapshot` covers this point.
> - A `RuntimeState` enum is still not introduced -- the run state is expressed with
>   `Session.running: bool` (`is_running(name)`), which suffices; the full `Idle<->Running` state
>   machine is left to be done together with the parallel scheduling of decisions 7/8.

**This shape conveniently eliminates two current hacks**:

- The TUI's `take_agent` / `restore_agent` serial technique -> becomes the `Idle <-> Running`
  transition of `Session.state`, with **each session taking / restoring its own**;
- The desktop's `Arc<Mutex<Option<Agent>>>` global single slot -> one slot per `Session`, and "agent is
  running" changes from a **global rejection** to a **rejection only for that session**.

**Decision 5: `Bootstrap` changes from "produce one Agent" to "produce an Agent factory".**
(**landed**)

The original `Bootstrap::assemble` produced only a single `Agent` (`src/bootstrap.rs`); it is now
renamed `AgentFactory::assemble`, and its product is **a factory + a shared `session_catalog`**, with
multiple sessions **creating multiple `Agent`s per session**. The `build_agent(messages) -> Agent`
factory reuses the same `ModelConfig` / tool registration / system prompt / working directory /
compression instruction:

- `assemble` returns **the factory + the shared `session_catalog`** (now `session_catalog` is already
  `Arc`-shared, with TUI / desktop reading the same `<config_dir>/shirley/workspaces/<workspace-id>/sessions`).
- The factory must **create a new `ToolManager` and re-register tools each time** (`ToolManager` is not
  `Clone`), but the `on_register` hooks (such as `web_search`'s credential injection) run once each --
  acceptable, because state is isolated per tool instance anyway.
- Note `reqwest::Client`: currently `Client::new()` is called inside each `run_stream`, so
  multi-session parallelism = multiple connection pools. A `Client` could be shared in the factory (a
  follow-up optimization, non-blocking).

---

**4. Persistence: tier A needs zero changes, tier B needs a lock**

**A (different sessions in parallel)**: different sessions write **different files**, and each
`JsonlSessionStore`'s own `Mutex<File>` is enough, **zero changes**.

**B (multiple drivers on the same session)**: when two `JsonlSessionStore` instances point at the same
file, **each holds an independent `File` handle and an independent `Mutex`, with no cross-instance
mutual exclusion** -> risk of interleaved writes; and `truncate`'s "write tmp -> rename -> reopen the
append handle" will overwrite each other under concurrency (`src/session.rs`'s `rewrite_locked`).

**Decision 6: this round only guarantees tier A safety; tier B is recorded as a follow-up, and when
needed, add a "per-session-name in-process lock table" to `FileSessionCatalog`
(`Arc<Mutex<HashMap<name, Arc<Mutex<()>>>>>`), plus a cross-process file lock if necessary.**

Reason: tier B (the same session written by multiple drivers at once) is **not a real scenario** in a
single-machine, single-process TUI / desktop -- a session should only have one active `Agent` at any
moment (decision 1). What really needs to be guarded against is "two windows opening the same session",
which is a **multi-process** problem that a lock table cannot solve either, requiring a file lock. So
do not design for B now, only **explicitly declare it unsupported**.

---

**5. Data flow (tier A)**

```
SessionCatalog.list()  -->  [SessionEntry...]  (directory scan, synchronous)
        │
        ├─ new / open ──►  Arc<dyn SessionStore>
        │                        │
        │                        ▼
        │            store.load() ──► Bootstrap::build_agent(messages) ──►  Agent
        │                        │
        │                        ▼
        │            Session { id, agent, items, ... }  ──►  sessions: HashMap
        │
        └─ switch the active pointer ──►  SessionManager.active = id  (a pure UI operation, does not rebuild the Agent)

user submits ──►  the active session's Agent::run_stream(task)
                │
                ▼
        one mpsc channel per session (route A)
                │
                ▼
        SessionManager writes events back to the corresponding Session by session_id
                │
                ▼
        rendering draws only the active session; background sessions advance as usual (progress can be shown on the tab)
```

---

**6. Parallel scheduling**

**Decision 7: one `tokio::spawn` task per session; `JoinHandle` / recycling stored per session.**

Change the current serial `take_agent` / `restore_agent` mode to each `Session` spawning / recycling on
its own:

- On submit, move the session's `Agent` into a background task (`RuntimeState::Running`), and put the
  `Agent` back when the task ends (`RuntimeState::Idle`);
- Consistent with the status quo: during a round, that session's `Agent` is not in the `Session` (it
  has been moved into the background task), so **the same session is not driven concurrently**;
- Different sessions' `Agent`s are not shared, and can truly run in parallel.

**Decision 8: there must be a "maximum number of parallel sessions" limit.**

Multi-session parallelism = multiple `reqwest::Client`s (multiple connection pools) + multiple
concurrent tool executions (`bash` really starts subprocesses). Without a limit, it will blow up the
connection count / process count / upstream rate limiting. Suggested:

- The default limit (e.g. 4) is configurable;
- When over the limit, **queue** rather than reject: a newly submitted session enters the pending
  queue and runs when a slot frees up;
- Keep the existing retry semantics for `ErrorKind::RateLimited` (429) (`SdkError::is_retryable`).

---

**7. Relationship to existing seams**

**Decision 9 (remove `switch_session`): `Agent::switch_session` is deleted from the SDK; "which
session is active" is application-layer orchestration, and the SDK should not have the concept of a
"current session".**

**Why it existed in the first place**: it was a product of the single-session era -- back then an
`Agent` served only one session, and the easiest way for `/session` to "change sessions" was to give
the `Agent` a different log source (clear `messages` / `todo` -> recover from the new log -> pin the
system on top). It misplaced "an application-layer orchestration action" (switching) into the SDK,
because at the time there was no `SessionManager` to carry the orchestration.

**Draw the boundary clearly (to avoid collateral damage)**:

- **Kept in the SDK at the time**: the `SessionStore` contract, the
  `Agent.session: Option<Arc<dyn SessionStore>>` field, and the recovery logic of "recover the working
  set from the log" -- the judgment then was "persistence is a legitimate SDK capability".
  **Later (a further evolution of `docs/session.md` decision 2)**: this layer was also moved out of the
  SDK -- session persistence is entirely owned by the application layer, the `Agent` no longer holds a
  `session` field and only `yield MessageAdded`; recovery is changed to the application layer's
  `store.load()` then `build_agent(log)`, and persistence is application-layer event-driven.
- **Moved out of the SDK**: the `switch_session` method + this round's further removal of the
  `SessionStore` contract / `session` field / recovery / persistence / tail truncation.
  `switch_session` expresses "**replace the current session**", whereas a "current session" does not
  exist in the SDK at all -- each `Agent` corresponds to a fixed log source, and switching is done by
  the `SessionManager.active` pointer (see decision 4).

**Migration surface**:

- SDK side: delete the `switch_session` method and its doc comment in `runtime/agent.rs`, and delete
  the corresponding unit tests (`agent.rs`'s `switch_session_clears_ledger`, and the two
  `switch_session_*` in `tests/session_contract.rs`);
- Application layer: the TUI's `/session` (`app.rs`) and the desktop's `agent_switch_session` are
  changed to **switching the `active` pointer via `SessionManager`** -- each `Session` already holds
  its own ready `Agent`, so switching **does not rebuild the `Agent`** (rebuilding would lose the
  context of that session running in the background, and also violate "multi-`Agent` isolation");
- Recovery semantics (**application layer `store.load()` -> `build_agent(log)`** + `system_message`
  pinned on top) are **reused as-is** -- it is now the business of the `build_agent` factory's caller,
  not a "switch" action.

---

**8. Boundaries and invariants**

- **The same session has only one active `Agent` at any moment** (decision 1). This is the
  precondition of parallelism safety: `todo` / the session log are isolated per session, and there is
  no mutable state shared across sessions.
- **`AgentEvent` variant semantics are unchanged**. Multi-session does not need to add a source
  identifier to events (decision 2 solves it with `Session`'s own channel); if fields really need to be
  appended in the future, follow the existing convention of "append fields, old consumers ignore with
  `..`" (this is how `ToolStarted` / `ToolFinished` were added back then).
- **The prefix cache is unaffected**: multiple sessions each maintain their own prefix, without
  interfering; but concurrent requests share the upstream rate limit, so watch out for 429.
- **`RunResult.messages`'s `start_index`** is maintained by each `Agent` separately, and is safe as
  long as `Agent`s are not shared (`docs/README.md` principle 3 mentions that the slicing out-of-range
  precondition is "rebuilding the underlying container", which multiple `Agent`s do not trigger).
- **Persistence only guarantees tier A**; tier B (multiple drivers on the same session / multiple
  processes) is **explicitly unsupported**.

---

**9. Landing order**

1. **P1 · Application-layer state-model refactor** (the main body of work): `SessionManager`
   (`Session` + `HashMap` + the `active` pointer); **each `Session` holds its own event channel**
   (decision 2).
2. **P1 · `AgentFactory::build_agent` factory** (decision 5) -- making multiple `Agent`s constructible;
   session recovery is done by the caller first `store.load()` then `build_agent(log)`.
3. **P2 · Parallel scheduling + limit** (decisions 7 / 8): per-session spawn, queueing, run-state
   display.
4. **P2 · SDK cancellation seam** (decision 3) -- almost certainly needed when doing P2.
5. **P3 · Remove `switch_session` from the SDK** (decision 9): delete the method + unit tests, and
   change the application layer to switching `active` via `SessionManager`.
6. **P3 · Persistence lock** (decision 6, tier B) -- tier A does not need it; leave it for last.

**MVP path**: do not touch the SDK event types -> "one mpsc per session + one `Agent` per session +
`SessionManager`" achieves tier A's "multiple sessions running in parallel"; the cancellation seam is
added as a second step.

---

**10. Acceptance criteria**

- Submitting tasks to two different sessions at the same time, both can advance, with events not
  crossing (each written back to its own `Session`);
- When a background session completes, its `items` / `usage` are persisted correctly, and switching to
  it shows the full conversation;
- Submitting again to a session that is running is rejected (the same `Agent` is not driven
  concurrently), with the error message consistent with the status quo;
- Closing a background session / switching the active pointer does not affect the running of other
  sessions;
- When the maximum parallelism is exceeded, new tasks queue rather than fail;
- When different sessions run in parallel, their task ledgers / session logs **do not pollute each
  other**;
- (If decision 3 is done) cancellation can act precisely on a specified session, without affecting the
  others, and `StopReason::Cancelled` is really produced.
