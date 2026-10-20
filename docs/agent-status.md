**Shirley technical design · Agent status layer (runtime state reporting)**

> Status: **proposal, not landed.** Design derived from arXiv:2610.21538 (Agent Status
> Protocol / ASP v0.1) — see §1 for what we take and what we deliberately do not.
> This doc follows the same convention as `todo.md` / `memory.md` / `skills.md`:
> **the state layer lives in the application layer; the SDK contract does not change.**

---

## 1. What we take from ASP, and what we don't

ASP is a *specification proposal* (v0.1 working draft) for reporting agent state to a status
server. It has **no evaluation** — it argues necessity, expressiveness and implementability,
not superiority. So we do not adopt "ASP is better"; we adopt the parts of its **design** that
survive scrutiny, and only where Shirley has a real gap today.

**Take (design decisions, not the wire protocol):**

| # | ASP decision | Why it matters for Shirley |
| --- | --- | --- |
| A1 | State is **orthogonal dimensions**, not one enum | `lifecycle / health / task` can hold simultaneously ("online + degraded + working"). Shirley has none of this. |
| A2 | **Liveness is judged by the observer, not the observed** | A crashed agent cannot report its own crash. |
| A3 | **"Alive but not progressing"** is a first-class failure, detected by a progress counter | The most common long-task failure and today completely invisible. |
| A4 | **Authority is ordered** and every state carries `source` | `operator > guardrail > self`; self-report never overrides an external verdict. |
| A5 | **Content-free** state events | Render stream (with content) and status stream (no content) must be separate surfaces. |
| A6 | `stale` marking when the source is gone | Don't show a dead agent's last task state as if it were current. |
| A7 | Heartbeat timeout = **2.5 × interval, floor 2** | Mature distributed-systems constant; free to reuse. |

**Do not take:**

- **The protocol itself** (HTTP POST ingest + SSE status server + `agentId` URN + `ext` namespaces).
  Shirley is a **single-user local app**, not an enterprise agent fleet. There is no multi-tenant
  status server to talk to. Building the wire format now would be exactly the "abstract for
  abstraction's sake" the project warns against.
- **The `floor` dimension.** Shirley has no multi-party floor management. (Subagents are a tree,
  not a debate.)
- **The `governance` dimension as a state machine.** Shirley has no permission layer yet
  (`docs/security.md` `PermissionPolicy` not landed); a `quarantined` state with no enforcement
  behind it is a lie in the UI.

So: **we adopt the state model and the liveness rules; we do not adopt the transport.**

---

## 2. The gap this closes in Shirley today

| Today | Consequence |
| --- | --- |
| `AgentEvent` is a **render stream**: `ContentDelta` / `ReasoningDelta` / `ToolStarted` / `Usage`. No lifecycle, no health. | The UI knows "text is arriving" but not "this agent is rate-limited / stalled / at capacity". |
| `StopReason` defines `MaxStepsReached` / `Cancelled` but **never produces them** (known gap 9). | "Why did it stop" is not answerable structurally. |
| `Session.running: bool` + `waiting_since: Option<Instant>` | One bit. Cannot distinguish *waiting for the model* from *waiting for a tool* from *stuck*. |
| A hung tool call / a model that keeps returning empty deltas | Looks identical to "thinking hard". Only the 180 s progress deadline in the SDK catches some of it. |

This is precisely ASP §7.2.1: **liveness ≠ progress.** Shirley has neither, expressed as state.

---

## 3. Architecture: three layers, zero SDK contract change

```
SDK (unchanged facade)
  AgentEvent ──────────────┐   the ONLY observation source
                           │   (ContentDelta / ToolStarted / ToolFinished /
                           │    Usage / ContextUsage / Retrying / Finished ...)
                           ▼
Application layer  ┌──────────────────────────────────────────────┐
  (new)            │ StateKeeper  (per session, Arc, interior mut.)│
                   │  · reduce(AgentEvent) -> StateSnapshot        │
                   │  · tick()  (heartbeat + progress check)       │
                   │  · snapshot() -> StateSnapshot (read-only)    │
                   └───────────────┬──────────────────────────────┘
                                   │ 独立 broadcast channel（不塞进 AgentEventWire）
                 ┌─────────────────┼──────────────────┐
                 ▼                 ▼                  ▼
            TUI footer        desktop/web SSE     (future) audit log
```

**Why the application layer, not the SDK** — three independent reasons that all point the same way:

1. ASP §4.4: stateless / short-lived agents cannot report natively; the **hosting platform must
   report on their behalf**. Shirley's SDK is exactly that shape (`build_agent` per session, the
   `Agent` is anonymous, identity lives on `Session`). So the reporter is `Session` / `SessionManager`.
2. Project rule: "the SDK only keeps business-agnostic fundamentals". "What counts as degraded",
   "how long before stalled", "which copy to show" are **product trade-offs** — same argument that
   moved `todo.rs` and `memory/` out of the SDK.
3. Contract discipline: `AgentEvent` is already the SDK's only seam for observation. Adding a
   second SDK concept (`AgentState`) would widen the facade for something the app can derive.

**Non-negotiable:** `StateKeeper` is a **pure reducer over `AgentEvent` + a tick**. It never
mutates the `Agent`, never touches `self.messages`, never sends anything to the model. It is a
**read-only projection** — so it cannot break the prefix cache, compaction, or the ReAct loop.

---

## 4. State model (Shirley's cut, not ASP's full vocabulary)

We keep three dimensions. `floor` and `governance` are dropped (§1); `task` is **reused from the
existing `TodoStore`** rather than duplicated.

```rust
// src/status.rs  (new)

/// How alive the turn is. Server/judge-assigned, never self-reported by the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Idle,          // no turn running
    Running,       // a turn is in flight and making progress
    Stalled,       // running, but progressSeq has not advanced for `stall_after`
    Unreachable,   // the driver vanished (panic / task dropped / no heartbeat)
}

/// What condition the agent is in. Free transitions (ASP §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Healthy,
    /// A tool call has been outstanding longer than `slow_tool_after`.
    SlowTool { name: String, elapsed_ms: u64 },
    /// Model call is retrying (driven by `AgentEvent::Retrying`).
    Retrying { attempt: usize, reason: String },
    /// Context compaction in flight (`CompressionStarted` / `CompressionFinished`).
    Compacting,
    /// Last turn ended in an error.
    Failed { kind: Option<ErrorKind>, message: String },
}

/// Where the current step is spending time. Hint only — never changes liveness.
/// Mirrors ASP's `activity` codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    ModelCall,
    ToolCall,
    Compacting,
    WaitingInput,
}

#[derive(Debug, Clone)]
pub struct StateSnapshot {
    pub liveness: Liveness,
    pub health: Health,
    pub activity: Option<Activity>,
    /// Monotonic progress counter: +1 per completed model call and per completed tool call.
    pub progress_seq: u64,
    /// When `progress_seq` last advanced (for the UI's "Worked for 20s").
    pub last_progress_at: Option<Instant>,
    /// Task summary mirrored from `TodoStore` — **not owned here** (§5.3).
    pub task: Option<TaskSummary>,
    /// True when the source that produced the rest of this snapshot is gone (ASP `stale`).
    pub stale: bool,
}
```

**Deliberately NOT modeled:**

- `at-capacity` — Shirley runs one turn per session; concurrency is expressed by
  `SessionManager` holding several sessions, not by one agent reporting spare capacity.
- `quota-exhausted` / `rate-limited` as first-class states — these arrive as `Retrying { reason }`
  today; promoting them to health states is a **P2** item gated on `ErrorKind` actually carrying
  them (it does: `RateLimited` — see §6.2).
- `paused` / `draining` / `stopping` — TUI/desktop cancellation is `Cancelled`, which the SDK does
  not produce yet (gap 9). Wire this **after** gap 9 lands, not before.

---

## 5. Mechanics

### 5.1 Progress counter (A3 — the core mechanism)

```rust
impl StateKeeper {
    /// Called by the turn driver for every `AgentEvent`, before `Session::apply_event`.
    pub fn on_event(&self, event: &AgentEvent) {
        let mut s = self.inner.lock();
        match event {
            // A completed model call is one unit of progress.
            AgentEvent::Usage(_) => s.bump_progress(Activity::ModelCall),
            // A completed tool call is one unit of progress.
            AgentEvent::ToolFinished { .. } => s.bump_progress(Activity::ToolCall),
            // Intent only: sets `activity`, does NOT bump progress.
            AgentEvent::ToolStarted { name, .. } => s.activity = Some(Activity::ToolCall),
            AgentEvent::ContentDelta(_) | AgentEvent::ReasoningDelta(_) => {
                s.activity = Some(Activity::ModelCall);
            }
            AgentEvent::Retrying { attempt, reason } => {
                s.health = Health::Retrying { attempt: *attempt, reason: reason.clone() };
            }
            AgentEvent::CompressionStarted => { s.health = Health::Compacting; s.activity = Some(Activity::Compacting); }
            AgentEvent::CompressionFinished => { s.health = Health::Healthy; }
            AgentEvent::Finished(result) => { /* liveness -> Idle, health from stop_reason */ }
            _ => {}
        }
    }
}
```

Rules (straight from ASP §7.2.1, translated):

- `progress_seq` advances **only** on a *completed* model call or tool call — **not** on deltas.
  Deltas prove bytes are flowing, not that work is advancing. This is what separates
  "long generation" from "stuck".
- `Stalled` is assigned only when `liveness == Running` **and** `progress_seq` has not advanced for
  `stall_after` **and** we are not in a by-design wait (`WaitingInput`).
- The state that is *waiting by design* does **not** trigger stalled — ASP is explicit about this
  (`input-required` / `awaiting-approval` / `delegated` / `queued` don't trigger `stalled`).

### 5.2 Heartbeat / stall detection (A2 + A7)

The driver already holds a `tokio::select!` loop. Add one interval arm:

```rust
// TUI main loop and ProjectRuntimeManager::spawn_turn both get this.
let mut hb = tokio::time::interval(Duration::from_secs(1));
loop {
    tokio::select! {
        Some(ev) = rx.recv() => { keeper.on_event(&ev); sessions.apply_event(name, ev); }
        _ = hb.tick() => { keeper.tick(); /* may flip Running -> Stalled */ }
    }
}
```

Constants (ASP's numbers, expressed for a single-user app):

| Constant | Default | Rationale |
| --- | --- | --- |
| `TICK` | 1 s | UI granularity for "Worked for Ns". |
| `stall_after` | 120 s | ASP says set **per agent / per task type** — image/3D generation legitimately takes minutes. Start generous; make it a setting. |
| `unreachable_after` | 2.5 × `stall_after` | ASP's 2.5 multiplier, floor 2. **If the driver task disappears (panic / dropped), mark `Unreachable` and `stale = true`.** |
| `slow_tool_after` | 30 s | Cosmetic: surface "running `bash` for 45s" instead of a spinner. |

> The SDK's existing 180 s model-progress deadline stays as-is; this layer is **observational**,
> not a second timeout authority. Do not duplicate cancellation logic here.

### 5.3 Task dimension = the existing `TodoStore`

Do **not** copy task state. `Session.todo: Option<Arc<TodoStore>>` already exists and is shared with
the `todo` tool. `StateKeeper::snapshot()` reads `TodoStore::view()` and projects a `TaskSummary`
(counts + current step). One writer (`todo` tool), one reader (status), no dual source of truth.

### 5.4 Authority + source (A4)

Even without a permission layer, keep the seam: every field that can be set from more than one
place carries a `source`. Today there is only one writer (`StateKeeper` itself, from `AgentEvent`),
so `source` is always `Observed`. The field exists so that when a `PermissionPolicy` lands
(`docs/security.md`), an operator/guardrail verdict can be layered on **without a schema change**,
with the precedence rule already fixed: `operator > guardrail > observed`.

### 5.5 Content-free (A5)

`StateSnapshot` contains **no prompt text, no model output, no tool arguments**. `Health::Failed`
carries `ErrorKind` (structural) and a *short* message — reuse the existing
`ToolFinished.error_kind: Option<ErrorKind>` field, which was added for exactly this reason.
This is why the status channel can be safely exposed to a remote/cloud client (see `cloud-server.md`)
without widening the content-exposure surface that the render stream already has.

### 5.6 Stale marking (A6)

When a turn's driver task is dropped or panics, the `Session` keeps the last `StateSnapshot` but
sets `stale = true`. The UI renders it muted ("last known: working · stale"). Never show a dead
turn's state as live. (The desktop already has panic recovery that emits a terminal event —
`docs/runtime-recovery.md` — so in the normal path `Unreachable` is only a safety net.)

---

## 6. Integration points (all in the application layer)

| File | Change | Risk |
| --- | --- | --- |
| `src/status.rs` **(new)** | `Liveness` / `Health` / `Activity` / `StateSnapshot` / `StateKeeper` + unit tests | none (new module) |
| `src/bootstrap.rs` | `BuiltAgent` gains `pub status: Arc<StateKeeper>`; `build_agent` creates one per session — **exactly the existing `todo` / `memory` / `skills` pattern** | low, mechanical |
| `src/interface/session/mod.rs` | `Session` gains `pub status: Option<Arc<StateKeeper>>`; `Session::apply_event` forwards to `keeper.on_event` **before** matching. **Retire the four hand-maintained flags** (see §6.1) | low; single call site shared by all three interfaces |
| `src/interface/tui/mod.rs` | `tokio::select!` gains the heartbeat arm; footer reads `keeper.snapshot()` | low |
| `src/interface/runtime/mod.rs` (`spawn_turn`) | same heartbeat arm; publish snapshot on the **status** channel | medium — second channel to wire |
| `src/interface/desktop/wire.rs` | new `StateSnapshotWire` + `status://event` event, **separate from `agent://event`** | low if separate; **do not** add a variant to `AgentEventWire` |
| `src/interface/desktop/web/` | footer / status pill subscribes to `status://event` | frontend only |
| `crates/shirley-agent-sdk/` | **nothing** | — |

### 6.1 This is a state-machine consolidation, not a new state machine

The runtime state flags in `Session` are **single fields with many writers** today. Verified by grep:

| Field | Write sites | Note |
| --- | --- | --- |
| `running` | `session.rs:944` (true) / `:954` (false) | its own doc comment says *"Synonymous with `agent.is_none()`"* — the same fact stored twice, kept in sync by hand |
| `waiting` / `waiting_since` | `app.rs:594/595` (set), `:658/659` (clear), `session.rs:311/312` (clear) | 3 writers, 1 reader |
| `compressing` | `session.rs:301/310/454/506/510` + `app.rs:1410` | **6 writers** for a yes/no answer |

All four are derivable from the `AgentEvent` stream. So `StateKeeper` must become their **sole writer**, and
the old fields must be **demoted to read-only accessors**:

```rust
// Session: delete the four fields, expose derived accessors instead.
pub fn running(&self) -> bool {
    self.status.as_ref().map_or(false, |s| s.snapshot().liveness != Liveness::Idle)
}
pub fn compressing(&self) -> bool {
    self.status.as_ref().map_or(false, |s| s.snapshot().health == Health::Compacting)
}
// waiting / waiting_since -> derived from activity + last_progress_at
```

**Hard rule — do not skip this.** Adding `StateKeeper` *while keeping* the old write sites produces
**three** representations of the same fact (`agent.is_none()` / `running` / `liveness`) — strictly worse
than today. The consolidation only pays off if the old writers are deleted.

**Measurable acceptance for this part:**

```sh
grep -rn '\.running = \|\.waiting = \|\.waiting_since = \|\.compressing = ' src/
# expected: no output
```

**Scope boundary — do not over-reach.** Only the four runtime flags are consolidated. These stay as they are:

| Field | Why it cannot be derived |
| --- | --- |
| `interrupt_requested` | user **intent** (Esc), not an observation |
| `streaming_delta_start` | an index into `items`, tied to render structure |
| `last_usage` / `total_usage` / `context_usage` | usage **data**, not a state machine |
| `items` / `scroll` / `store` / `todo` | each belongs to its own concern |

**Why a separate channel and not an `AgentEventWire` variant:** `AgentEventWire` is the *render*
contract (it carries content). The status stream is content-free and has a different lifecycle
(it ticks even when no `AgentEvent` arrives — that's the whole point of stall detection). Merging
them would force the render channel to carry heartbeats and would violate A5.

---

## 7. Phasing

| Phase | Scope | Acceptance |
| --- | --- | --- |
| **P0** | `src/status.rs` + `StateKeeper` reducer + unit tests; `Session.status`; **retire `running` / `waiting` / `waiting_since` / `compressing` (§6.1)**; TUI footer reads the snapshot | (a) reducer test: deltas do **not** bump `progress_seq`; `Usage`/`ToolFinished` do; `stall_after` flips `Running -> Stalled`; a by-design wait never stalls. (b) `grep -rn '\.running = \|\.waiting = \|\.waiting_since = \|\.compressing = ' src/` returns **nothing**. |
| **P1** | Heartbeat arm in TUI + `spawn_turn`; `Unreachable` on driver drop; stale rendering | Kill a turn's task mid-run → UI shows "stale", not "working". |
| **P2** | Desktop/web `status://event` channel + status pill; "Worked for Ns" derived from `last_progress_at` | Same snapshot visible in TUI and web; content-free payload verified by a serialization test. |
| **P3 (gated)** | Promote `RateLimited` / `QuotaExhausted` to `Health` from `ErrorKind`; wire `Cancelled` once SDK gap 9 lands | Only after `StopReason::Cancelled` is actually produced. |

P0+P1 are the whole value; P2 is reach; P3 is cleanup. **Do not start at P3.**

**P3 gate re-verified (implementation time).** The gate is *not* met, so P3 stays gated:

- `grep -rn 'StopReason::Cancelled' crates/shirley-agent-sdk/src/` → **no output**: the SDK main
  loop never produces `Cancelled` (cancellation is still synthesized in the app layer's
  `runtime.rs::cancelled_event()`). Wiring `Cancelled` into `Health`/`Liveness` now would be
  describing a state the SDK cannot actually reach.
- `ErrorKind` has **no `QuotaExhausted`** variant today (`Transport` / `RateLimited` /
  `ServerError` / `BadRequest` / `Unsupported` / `ToolFailure` / `Internal`). Promoting a
  non-existent variant to `Health` would mean inventing it in the SDK first — out of scope here.
- `RateLimited` *does* exist and already flows through `Retrying { reason }`; promoting it to a
  dedicated `Health` variant is cosmetic (no new information, no new behaviour), so it is folded
  into P3 rather than pulled forward.

Revisit P3 only after `docs/sdk-gaps.md` gap-9 lands (`StopReason::Cancelled` /
`MaxStepsReached` produced by the SDK main loop) and `ErrorKind` carries a quota variant.

---

## 8. What this explicitly does not do

- **No `floor` dimension** — no multi-party conversations.
- **No `governance` enforcement** — the field seam exists, the enforcement does not.
- **No network protocol** — no HTTP ingest, no status server, no `agentId` URN. If Shirley ever
  grows a fleet (it does not today), revisit ASP §8 then, and the state model above maps 1:1.
- **No second timeout authority** — the SDK's model deadline and the sandbox timeout remain the
  only things that actually *stop* work. `StateKeeper` only *describes*.
- **No change to `AgentEvent`, `Message`, `StopReason`, or any `pub use` in `lib.rs`.**

---

## 9. Test plan (matching `docs/testing.md`'s style)

Reducer tests (pure, no async, no network):

1. `deltas_do_not_count_as_progress` — 1000 `ContentDelta`s leave `progress_seq == 0`.
2. `completed_calls_bump_progress` — one `Usage` + one `ToolFinished` → `progress_seq == 2`.
3. `stall_requires_running_and_no_progress` — `Idle` never stalls.
4. `by_design_wait_never_stalls` — `Activity::WaitingInput` suppresses `Stalled`.
5. `retrying_sets_health_not_liveness` — `Retrying` changes `Health` only.
6. `finished_resets_liveness_to_idle` — including `Failed` health from an `Err` terminal.
7. `snapshot_is_content_free` — serialize `StateSnapshot`, assert no `ContentDelta`/tool-argument
   bytes appear (regression guard for A5).
8. `task_summary_mirrors_todo_store` — one writer, no duplication.

Integration (one per interface):

9. TUI: start a turn, feed a fake slow stream, assert the footer shows `Stalled` after `stall_after`.
10. `spawn_turn`: drop the driver task, assert `Unreachable` + `stale == true`.

---

## 10. Relationship to existing docs

- `docs/todo.md` — the `task` dimension reads from `TodoStore`; this doc does **not** duplicate it.
- `docs/runtime-recovery.md` — panic recovery emits a terminal event; this layer turns that into
  `Unreachable` + `stale` instead of silently freezing.
- `docs/security.md` — the `source` / precedence seam (§5.4) is where `PermissionPolicy` will plug in.
- `docs/cloud-server.md` — the content-free status stream (§5.5) is safe to expose remotely; the
  render stream's content exposure is unchanged.
- `docs/sdk-gaps.md` gap-9 — `StopReason::Cancelled` / `MaxStepsReached` must be produced before P3.
