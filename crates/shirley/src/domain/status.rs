//! Agent runtime status layer: a **read-only projection** of "is this turn alive, healthy, and
//! making progress" derived from the SDK's `AgentEvent` stream plus a heartbeat tick.
//!
//! **This is an application-layer capability, not an SDK base capability.** The SDK only emits
//! `AgentEvent` (a *render* stream that carries content); it has no concept of lifecycle/health.
//! "What counts as degraded / how long before stalled / what to show" are product tradeoffs — the
//! same argument that moved `todo.rs` and `memory/` out of the SDK. So the SDK facade is unchanged.
//!
//! Design (see `docs/agent-status.md`):
//!
//! - **`progress_seq` advances only on a *completed* model call or tool call** — never on a
//!   `ContentDelta`. Deltas prove bytes are flowing, not that work is advancing. This is what
//!   separates "long generation" from "stuck".
//! - **`StateKeeper` is the single writer** of the session's runtime state. It replaces the four
//!   hand-maintained flags (`running` / `waiting` / `waiting_since` / `compressing`) that used to be
//!   written from ~14 scattered sites; those are now derived accessors on `Session` (docs §6.1).
//! - **Content-free**: nothing here stores prompt text, model output, or tool arguments.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shirley_agent_sdk::{AgentEvent, ErrorKind, StopReason};

/// Default "alive but not progressing" threshold. Generous on purpose: image / 3D generation and
/// long compactions legitimately take minutes (ASP §7.2.1 says set it per agent / per task type).
pub const DEFAULT_STALL_AFTER: Duration = Duration::from_secs(120);

/// Default "a single tool call is taking suspiciously long" threshold. Cosmetic only — it changes
/// the label, never the liveness verdict.
pub const DEFAULT_SLOW_TOOL_AFTER: Duration = Duration::from_secs(30);

/// How alive the turn is. Assigned by the observer (this module), never self-reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// No turn in flight.
    Idle,
    /// A turn is in flight and has advanced `progress_seq` within `stall_after`.
    Running,
    /// A turn is in flight but `progress_seq` has not advanced for `stall_after`.
    Stalled,
    /// The driver task vanished (panic / dropped) — the safety net, not the normal path.
    Unreachable,
}

/// What condition the agent is in. Free transitions; never changes `Liveness` on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Healthy,
    /// One tool call has been outstanding longer than `slow_tool_after`.
    SlowTool {
        name: String,
        elapsed_ms: u64,
    },
    /// The model call is retrying (driven by `AgentEvent::Retrying`).
    Retrying {
        attempt: usize,
        reason: String,
    },
    /// Context compaction is in flight.
    Compacting,
    /// The last turn ended in a failure.
    Failed {
        kind: Option<ErrorKind>,
        message: String,
    },
}

/// Where the current step is spending time. A hint only — it never changes `Liveness`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    ModelCall,
    ToolCall,
    Compacting,
    /// Waiting **by design** (e.g. the login / rewind-edit input states). Suppresses `Stalled`.
    WaitingInput,
}

/// A content-free snapshot for the UI / status channel. Contains **no** prompt, output, or
/// tool-argument text.
#[derive(Debug, Clone)]
pub struct StateSnapshot {
    pub liveness: Liveness,
    pub health: Health,
    pub activity: Option<Activity>,
    /// Monotonic within a turn: +1 per completed model call and per completed tool call.
    pub progress_seq: u64,
    /// When `progress_seq` last advanced.
    pub last_progress_at: Option<Instant>,
    /// When the current turn started.
    pub turn_started_at: Option<Instant>,
    /// True when the source that produced the rest of this snapshot is gone.
    pub stale: bool,
}

struct Inner {
    liveness: Liveness,
    health: Health,
    activity: Option<Activity>,
    progress_seq: u64,
    last_progress_at: Option<Instant>,
    turn_started_at: Option<Instant>,
    /// When *any* event (including a content/reasoning delta) last arrived. Liveness uses this:
    /// a long streaming generation is alive, even though it advances no `progress_seq`.
    last_event_at: Option<Instant>,
    stale: bool,
    /// The tool call currently in flight (name + start), for slow-tool detection.
    current_tool: Option<(String, Instant)>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            liveness: Liveness::Idle,
            health: Health::Healthy,
            activity: None,
            progress_seq: 0,
            last_progress_at: None,
            turn_started_at: None,
            last_event_at: None,
            stale: false,
            current_tool: None,
        }
    }
}

/// Per-session runtime state machine, shared via `Arc`.
///
/// Interior mutability (`Mutex`) because the driver mutates it while the UI reads snapshots from
/// another task/frame. Every mutation goes through a method here — there is no public field.
pub struct StateKeeper {
    inner: Mutex<Inner>,
    stall_after: Duration,
    slow_tool_after: Duration,
}

impl Default for StateKeeper {
    fn default() -> Self {
        Self::new()
    }
}

impl StateKeeper {
    pub fn new() -> Self {
        Self::with_thresholds(DEFAULT_STALL_AFTER, DEFAULT_SLOW_TOOL_AFTER)
    }

    /// Test / configuration seam: shrink the thresholds so stall / slow-tool paths are reachable
    /// without sleeping for two minutes.
    pub fn with_thresholds(stall_after: Duration, slow_tool_after: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            stall_after,
            slow_tool_after,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A new turn begins. Resets the per-turn progress counter.
    pub fn begin_turn(&self) {
        let mut s = self.lock();
        let now = Instant::now();
        s.liveness = Liveness::Running;
        s.health = Health::Healthy;
        s.activity = Some(Activity::ModelCall);
        s.progress_seq = 0;
        s.turn_started_at = Some(now);
        s.last_progress_at = Some(now);
        s.last_event_at = Some(now);
        s.stale = false;
        s.current_tool = None;
    }

    /// The turn is over (normal completion, cancellation, or the agent returned). Idempotent.
    pub fn end_turn(&self) {
        let mut s = self.lock();
        s.liveness = Liveness::Idle;
        s.activity = None;
        s.current_tool = None;
    }

    /// Reduce one SDK event into the state.
    pub fn on_event(&self, event: &AgentEvent) {
        let mut s = self.lock();
        // Any event (delta included) proves the turn is alive. Deltas do *not* advance
        // `progress_seq` (they don't prove work is advancing), but they do keep liveness fresh.
        s.last_event_at = Some(Instant::now());
        if s.liveness == Liveness::Stalled {
            s.liveness = Liveness::Running;
        }
        match event {
            // Deltas: set the activity hint but **do not** count as progress.
            AgentEvent::ContentDelta(_) | AgentEvent::ReasoningDelta(_) => {
                s.activity = Some(Activity::ModelCall);
            }
            // A completed model call = one unit of progress.
            AgentEvent::Usage(_) => {
                s.activity = Some(Activity::ModelCall);
                bump_progress(&mut s);
            }
            AgentEvent::ToolStarted { name, .. } => {
                s.activity = Some(Activity::ToolCall);
                s.current_tool = Some((name.clone(), Instant::now()));
            }
            // A completed tool call = one unit of progress. A failed tool is *not* a failed turn
            // (that is the normal ReAct loop), so health is left alone.
            AgentEvent::ToolFinished { .. } => {
                s.activity = Some(Activity::ToolCall);
                s.current_tool = None;
                if matches!(s.health, Health::SlowTool { .. }) {
                    s.health = Health::Healthy;
                }
                bump_progress(&mut s);
            }
            AgentEvent::Retrying { attempt, reason } => {
                s.health = Health::Retrying {
                    attempt: *attempt,
                    reason: reason.clone(),
                };
            }
            AgentEvent::CompactionStarted => {
                s.health = Health::Compacting;
                s.activity = Some(Activity::Compacting);
            }
            AgentEvent::CompactionFinished => {
                if matches!(s.health, Health::Compacting) {
                    s.health = Health::Healthy;
                }
            }
            AgentEvent::Finished(result) => {
                finish(&mut s, &result.stop_reason);
            }
            // No state impact: message/notice/context-usage are view concerns.
            AgentEvent::MessageAdded(_)
            | AgentEvent::Notice(_)
            | AgentEvent::ContextUsage { .. } => {}
        }
    }

    /// The turn ended in a runtime error (the `Err` branch of the event stream).
    pub fn on_error(&self, message: &str) {
        let mut s = self.lock();
        s.liveness = Liveness::Idle;
        s.activity = None;
        s.current_tool = None;
        s.health = Health::Failed {
            kind: None,
            message: message.to_owned(),
        };
    }

    /// Mark the session as "waiting by design" (login / rewind edit). Suppresses `Stalled`.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "waiting-by-design flag not yet driven by the UI")
    )]
    pub fn set_waiting_input(&self, waiting: bool) {
        let mut s = self.lock();
        if waiting {
            s.activity = Some(Activity::WaitingInput);
        } else if s.activity == Some(Activity::WaitingInput) {
            s.activity = None;
        }
    }

    /// The driver task vanished. Safety net only.
    pub fn mark_unreachable(&self) {
        let mut s = self.lock();
        s.liveness = Liveness::Unreachable;
        s.stale = true;
        s.current_tool = None;
    }

    /// Heartbeat: re-evaluate time-based transitions (slow tool, stalled). Call ~1 Hz.
    pub fn tick(&self) {
        let mut s = self.lock();
        let now = Instant::now();

        if let Some((name, started)) = &s.current_tool {
            let elapsed = now.duration_since(*started);
            if elapsed >= self.slow_tool_after && matches!(s.health, Health::Healthy) {
                s.health = Health::SlowTool {
                    name: name.clone(),
                    elapsed_ms: elapsed.as_millis() as u64,
                };
            }
        }

        if s.liveness == Liveness::Running
            && s.activity != Some(Activity::WaitingInput)
            && s.last_event_at
                .is_some_and(|t| now.duration_since(t) >= self.stall_after)
        {
            s.liveness = Liveness::Stalled;
        }
    }

    // ---- Derived accessors (the only way the UI asks "is it running / compressing") ----

    #[expect(
        dead_code,
        reason = "wire.rs reads the `liveness` field directly; accessor kept for completeness"
    )]
    pub fn liveness(&self) -> Liveness {
        self.lock().liveness
    }

    /// True while a turn is in flight (Running or Stalled). Replaces the old `running` / `waiting`.
    pub fn is_active(&self) -> bool {
        !matches!(self.lock().liveness, Liveness::Idle)
    }

    pub fn is_compacting(&self) -> bool {
        matches!(self.lock().health, Health::Compacting)
    }

    /// Seconds since the current turn started (the "replying Ns" label).
    pub fn elapsed_seconds(&self) -> Option<u64> {
        self.lock()
            .turn_started_at
            .map(|t| Instant::now().duration_since(t).as_secs())
    }

    /// Seconds since `progress_seq` last advanced — the "last real progress" figure.
    #[expect(
        dead_code,
        reason = "UI reads the precomputed snapshot field; accessor kept for completeness"
    )]
    pub fn seconds_since_progress(&self) -> Option<u64> {
        self.lock()
            .last_progress_at
            .map(|t| Instant::now().duration_since(t).as_secs())
    }

    pub fn snapshot(&self) -> StateSnapshot {
        let s = self.lock();
        StateSnapshot {
            liveness: s.liveness,
            health: s.health.clone(),
            activity: s.activity,
            progress_seq: s.progress_seq,
            last_progress_at: s.last_progress_at,
            turn_started_at: s.turn_started_at,
            stale: s.stale,
        }
    }
}

/// RAII guard for a turn driver task.
///
/// Held by the spawned task that drives one turn. If the task is dropped (cancelled via
/// `JoinHandle::abort`, or the runtime shuts down mid-turn) or panics, `Drop` fires while still
/// armed and flips the keeper to `Unreachable` + `stale` — the "last known state" the UI renders
/// muted, never as live. On a clean end the driver calls [`TurnGuard::disarm`] first, so the drop
/// is a no-op and the normal `end_turn` verdict stands.
pub struct TurnGuard {
    keeper: Arc<StateKeeper>,
    armed: bool,
}

impl TurnGuard {
    pub fn new(keeper: Arc<StateKeeper>) -> Self {
        Self {
            keeper,
            armed: true,
        }
    }

    /// The turn ended through the normal path; do not mark it unreachable on drop.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        if self.armed {
            self.keeper.mark_unreachable();
        }
    }
}

fn bump_progress(s: &mut Inner) {
    s.progress_seq = s.progress_seq.saturating_add(1);
    s.last_progress_at = Some(Instant::now());
    // Any real progress rescues a stalled turn.
    if s.liveness == Liveness::Stalled {
        s.liveness = Liveness::Running;
    }
}

fn finish(s: &mut Inner, stop: &StopReason) {
    s.liveness = Liveness::Idle;
    s.activity = None;
    s.current_tool = None;
    s.health = match stop {
        StopReason::Completed | StopReason::Cancelled => Health::Healthy,
        StopReason::MaxStepsReached => Health::Failed {
            kind: Some(ErrorKind::Internal),
            message: "reached the execution limit for this turn".to_owned(),
        },
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use shirley_agent_sdk::{RunResult, Usage};

    fn keeper() -> StateKeeper {
        // Tiny thresholds so the time-based paths are reachable without sleeping for minutes.
        StateKeeper::with_thresholds(Duration::from_millis(30), Duration::from_millis(10))
    }

    fn finished(stop: StopReason) -> AgentEvent {
        AgentEvent::Finished(RunResult {
            messages: Vec::new(),
            stop_reason: stop,
            usage: Usage::default(),
        })
    }

    #[test]
    fn deltas_do_not_count_as_progress() {
        let k = keeper();
        k.begin_turn();
        for _ in 0..1000 {
            k.on_event(&AgentEvent::ContentDelta("x".repeat(64)));
            k.on_event(&AgentEvent::ReasoningDelta("y".repeat(64)));
        }
        assert_eq!(
            k.snapshot().progress_seq,
            0,
            "deltas must never count as progress"
        );
        assert_eq!(k.snapshot().activity, Some(Activity::ModelCall));
    }

    #[test]
    fn completed_calls_bump_progress() {
        let k = keeper();
        k.begin_turn();
        k.on_event(&AgentEvent::Usage(Usage::default()));
        k.on_event(&AgentEvent::ToolFinished {
            call_id: "c1".into(),
            name: "bash".into(),
            ok: true,
            output: String::new(),
            error_kind: None,
            elapsed_ms: 5,
        });
        assert_eq!(k.snapshot().progress_seq, 2);
    }

    #[test]
    fn stall_requires_running_and_no_progress() {
        let k = keeper();
        // Idle never stalls.
        k.tick();
        assert_eq!(k.snapshot().liveness, Liveness::Idle);

        k.begin_turn();
        std::thread::sleep(Duration::from_millis(40));
        k.tick();
        assert_eq!(k.snapshot().liveness, Liveness::Stalled);

        // Progress rescues it.
        k.on_event(&AgentEvent::Usage(Usage::default()));
        assert_eq!(k.snapshot().liveness, Liveness::Running);
    }

    #[test]
    fn by_design_wait_never_stalls() {
        let k = keeper();
        k.begin_turn();
        k.set_waiting_input(true);
        std::thread::sleep(Duration::from_millis(40));
        k.tick();
        assert_eq!(
            k.snapshot().liveness,
            Liveness::Running,
            "waiting by design must not be reported as stalled"
        );
    }

    #[test]
    fn retrying_sets_health_not_liveness() {
        let k = keeper();
        k.begin_turn();
        k.on_event(&AgentEvent::Retrying {
            attempt: 2,
            reason: "429".into(),
        });
        let snap = k.snapshot();
        assert_eq!(snap.liveness, Liveness::Running);
        assert!(matches!(snap.health, Health::Retrying { attempt: 2, .. }));
    }

    #[test]
    fn slow_tool_is_cosmetic() {
        let k = keeper();
        k.begin_turn();
        k.on_event(&AgentEvent::ToolStarted {
            call_id: "c1".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        });
        std::thread::sleep(Duration::from_millis(20));
        k.tick();
        let snap = k.snapshot();
        assert!(matches!(snap.health, Health::SlowTool { .. }));
        assert_eq!(
            snap.liveness,
            Liveness::Running,
            "slow tool does not change liveness"
        );

        // Finishing the tool clears the slow-tool health and bumps progress.
        k.on_event(&AgentEvent::ToolFinished {
            call_id: "c1".into(),
            name: "bash".into(),
            ok: true,
            output: String::new(),
            error_kind: None,
            elapsed_ms: 20,
        });
        assert_eq!(k.snapshot().health, Health::Healthy);
        assert_eq!(k.snapshot().progress_seq, 1);
    }

    #[test]
    fn finished_resets_liveness_to_idle() {
        let k = keeper();
        k.begin_turn();
        k.on_event(&finished(StopReason::Completed));
        let snap = k.snapshot();
        assert_eq!(snap.liveness, Liveness::Idle);
        assert_eq!(snap.health, Health::Healthy);

        // MaxStepsReached is a structural failure, not a healthy stop.
        k.begin_turn();
        k.on_event(&finished(StopReason::MaxStepsReached));
        assert!(matches!(k.snapshot().health, Health::Failed { .. }));
    }

    #[test]
    fn snapshot_is_content_free() {
        let k = keeper();
        k.begin_turn();
        let secret = "SUPER_SECRET_PROMPT_TEXT_12345";
        k.on_event(&AgentEvent::ContentDelta(secret.to_owned()));
        k.on_event(&AgentEvent::ToolStarted {
            call_id: "c1".into(),
            name: "bash".into(),
            arguments: format!("{{\"cmd\":\"{secret}\"}}"),
        });
        let rendered = format!("{:?}", k.snapshot());
        assert!(
            !rendered.contains(secret),
            "snapshot must never carry prompt / output / tool-argument content: {rendered}"
        );
    }

    #[test]
    fn dropped_turn_guard_marks_unreachable_and_stale() {
        let k = Arc::new(keeper());
        k.begin_turn();
        {
            let _guard = TurnGuard::new(Arc::clone(&k));
            // Dropped without disarm -> the safety net fires.
        }
        let snap = k.snapshot();
        assert_eq!(snap.liveness, Liveness::Unreachable);
        assert!(
            snap.stale,
            "a vanished driver must render as stale, never as live"
        );
    }

    #[test]
    fn disarmed_turn_guard_leaves_the_normal_verdict() {
        let k = Arc::new(keeper());
        k.begin_turn();
        let mut guard = TurnGuard::new(Arc::clone(&k));
        k.on_event(&finished(StopReason::Completed));
        guard.disarm();
        drop(guard);
        let snap = k.snapshot();
        assert_eq!(
            snap.liveness,
            Liveness::Idle,
            "a clean end must not be overwritten by the guard"
        );
        assert!(!snap.stale);
    }

    #[test]
    fn begin_turn_resets_progress_and_stale() {
        let k = keeper();
        k.mark_unreachable();
        assert!(k.snapshot().stale);
        k.begin_turn();
        let snap = k.snapshot();
        assert_eq!(snap.progress_seq, 0);
        assert!(!snap.stale);
        assert_eq!(snap.liveness, Liveness::Running);
    }
}
