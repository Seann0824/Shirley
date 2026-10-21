//! Permission layer (C 档体感): a transport-agnostic policy + approval broker.
//!
//! This is the application-side counterpart to the SDK's
//! [`shirley_agent_sdk::Approver`] seam. The SDK owns the *enforcement point* (it calls
//! `classify` before every tool and, on `Ask`, awaits `approve`); this module owns the
//! *policy* — which tool calls are safe, which need a human, which are refused — and the
//! *plumbing* that carries an approval prompt to whichever UI is driving (TUI / desktop /
//! server) and blocks until the human answers.
//!
//! Why a broker instead of a stream event: a tool call runs inside the runtime's
//! `FuturesUnordered`; while it is blocked on a human, the runtime's main loop is parked on
//! `tasks.next().await` and cannot `yield` anything. So approval must be **out-of-band**:
//! [`ApprovalBroker::approve`] pushes a prompt over a `broadcast` channel and awaits a
//! `oneshot` the UI resolves via [`ApprovalBroker::resolve`]. No transport code decides
//! policy; every transport only renders prompts and returns decisions.
//!
//! The three-way model (mirrors `docs/security.md` §5):
//! - [`Verdict::Allow`] — run it, no human needed.
//! - [`Verdict::Ask`] — pause and ask a human (this is the "muse 感").
//! - [`Verdict::Deny`] — refuse; the model reads the reason and changes approach.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use shirley_agent_sdk::{ApprovalFuture, Approver, Decision, PermissionRequest, Verdict};
use tokio::sync::{broadcast, oneshot};

/// Coarse capability ceiling, chosen once per deployment/profile.
///
/// The profile does **not** replace per-call classification — it modulates how a given
/// [`RiskClass`] is resolved. This is the knob that lets "cloud, but careful" and "cloud,
/// full power" share one code path: same tools, different ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityProfile {
    /// Read-only: safe reads run; anything that writes, reaches the network, or is
    /// destructive is refused outright (never even prompted — there is no one to ask on a
    /// headless cloud worker by default).
    ReadOnly,
    /// Workspace-write: safe reads and builds run; network and destructive actions pause for
    /// a human. This is the default "C 档体感" profile.
    WorkspaceWrite,
    /// Full: safe reads, builds, and network run automatically; only destructive /
    /// irreversible actions pause. Maximum capability with a human gate on the sharp edges.
    Full,
}

impl CapabilityProfile {
    /// Parse a profile name (case-insensitive) from config / env. Unknown → `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "readonly" | "read-only" | "read_only" => Some(Self::ReadOnly),
            "workspacewrite" | "workspace-write" | "workspace_write" | "workspace" => {
                Some(Self::WorkspaceWrite)
            }
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// Default when nothing is configured: the "C 档体感" middle ground.
    pub const fn default_profile() -> Self {
        Self::WorkspaceWrite
    }

    /// Resolve a risk class into a verdict under this profile.
    fn resolve(self, risk: RiskClass) -> Verdict {
        match (self, risk) {
            // Catastrophic is refused under every profile — a human "yes" is not enough for
            // `rm -rf /`. This is the one place policy is absolute.
            (_, RiskClass::Catastrophic) => Verdict::Deny {
                reason: "this command looks catastrophic and is refused regardless of profile; \
                         narrow it down and retry"
                    .to_string(),
            },
            (Self::ReadOnly, RiskClass::Safe) => Verdict::Allow,
            (Self::ReadOnly, RiskClass::Network | RiskClass::Destructive) => Verdict::Deny {
                reason: "the current capability profile is read-only; writes and network access \
                         are disabled"
                    .to_string(),
            },
            (Self::WorkspaceWrite, RiskClass::Safe) => Verdict::Allow,
            (Self::WorkspaceWrite, RiskClass::Network | RiskClass::Destructive) => Verdict::Ask,
            (Self::Full, RiskClass::Safe | RiskClass::Network) => Verdict::Allow,
            (Self::Full, RiskClass::Destructive) => Verdict::Ask,
        }
    }
}

/// Coarse risk of a single tool call, before the profile decides Allow/Ask/Deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RiskClass {
    /// Pure read / build / status: no external side effects beyond the workspace.
    Safe,
    /// Reaches the network or an external service.
    Network,
    /// Irreversible or broad-scope mutation (delete, force-push, install global, sudo, …).
    Destructive,
    /// Looks like it could destroy the machine / workspace wholesale.
    Catastrophic,
}

/// An approval prompt pushed to the UI. Carries the routing key so a multi-project /
/// multi-session desktop can show it on the right conversation.
#[derive(Debug, Clone)]
pub struct ApprovalPrompt {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: String,
    pub project: Option<String>,
    pub session: Option<String>,
}

/// The out-of-band approval broker. One per `Agent` (per session), created by
/// `bootstrap::build_agent` and shared with the tool context as an [`Approver`].
pub struct ApprovalBroker {
    policy: CapabilityProfile,
    /// `call_id → oneshot sender`. The runtime's `approve()` inserts before prompting and
    /// the UI's `resolve()` removes + fires. A missing entry (already resolved / timed out)
    /// makes a late `resolve()` a harmless no-op.
    pending: Mutex<HashMap<String, oneshot::Sender<Decision>>>,
    /// Fan-out of prompts to whichever UI subscribed. `broadcast` (not `mpsc`) so the driver
    /// can `subscribe()` a fresh receiver each turn without moving ownership.
    prompts: broadcast::Sender<ApprovalPrompt>,
    /// How long to wait for a human before denying. A hung approval must not stall a turn.
    timeout: Duration,
    /// Prompt routing key, set after the broker is built (the session name is only known
    /// once the `Agent` is inserted into a `Session`). `(project, session)`.
    route: Mutex<(Option<String>, Option<String>)>,
    /// Optional audit log (JSONL). Best-effort: a write failure never blocks a decision.
    audit: Option<PathBuf>,
}

impl std::fmt::Debug for ApprovalBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalBroker")
            .field("policy", &self.policy)
            .field("timeout", &self.timeout)
            .field("route", &self.route.lock().ok().map(|r| r.clone()))
            .finish_non_exhaustive()
    }
}

impl ApprovalBroker {
    /// Create a broker. `route` = (project, session) for prompt routing; `audit` = optional
    /// JSONL path.
    pub fn new(
        policy: CapabilityProfile,
        timeout: Duration,
        project: Option<String>,
        session: Option<String>,
        audit: Option<PathBuf>,
    ) -> Self {
        let (prompts, _) = broadcast::channel(64);
        Self {
            policy,
            pending: Mutex::new(HashMap::new()),
            prompts,
            timeout,
            route: Mutex::new((project, session)),
            audit,
        }
    }

    /// Build a default broker for the given route with the standard 120s approval timeout.
    #[cfg(test)]
    pub fn with_defaults(project: Option<String>, session: Option<String>) -> Self {
        Self::new(
            CapabilityProfile::default_profile(),
            Duration::from_secs(120),
            project,
            session,
            None,
        )
    }

    /// Subscribe to approval prompts (the driver calls this once per turn).
    pub fn subscribe(&self) -> broadcast::Receiver<ApprovalPrompt> {
        self.prompts.subscribe()
    }

    /// Set the routing key used to tag approval prompts. Called once the session name /
    /// project is known (the `Agent` is anonymous; identity lives on the `Session`).
    pub fn set_route(&self, project: Option<String>, session: Option<String>) {
        if let Ok(mut route) = self.route.lock() {
            *route = (project, session);
        }
    }

    /// Resolve a pending approval. Returns `true` if a live prompt was waiting.
    pub fn resolve(&self, call_id: &str, decision: Decision) -> bool {
        let sender = self.pending.lock().ok().and_then(|mut map| map.remove(call_id));
        if let Some(sender) = sender {
            let allowed = matches!(decision, Decision::Allow);
            self.audit(call_id, allowed);
            let _ = sender.send(decision);
            true
        } else {
            false
        }
    }

    /// Number of prompts currently awaiting a human (for UI / tests).
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.lock().map(|map| map.len()).unwrap_or(0)
    }

    fn audit(&self, call_id: &str, allowed: bool) {
        let Some(path) = &self.audit else {
            return;
        };
        let line = format!(
            "{{\"ts\":{},\"call_id\":{call_id:?},\"allowed\":{allowed}}}\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

impl Approver for ApprovalBroker {
    fn classify(&self, request: &PermissionRequest) -> Verdict {
        let risk = classify_call(&request.tool_name, &request.arguments);
        self.policy.resolve(risk)
    }

    fn approve<'a>(&'a self, request: &'a PermissionRequest) -> ApprovalFuture<'a> {
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            if let Ok(mut map) = self.pending.lock() {
                map.insert(request.call_id.clone(), tx);
            }
            let (project, session) = self
                .route
                .lock()
                .map(|route| route.clone())
                .unwrap_or((None, None));
            let prompt = ApprovalPrompt {
                call_id: request.call_id.clone(),
                tool_name: request.tool_name.clone(),
                arguments: request.arguments.clone(),
                project,
                session,
            };
            // If nobody is subscribed (headless, no UI), fail closed and fast.
            if self.prompts.send(prompt).is_err() {
                if let Ok(mut map) = self.pending.lock() {
                    map.remove(&request.call_id);
                }
                return Decision::Deny {
                    reason: "no approval UI is attached; the action was refused".to_string(),
                };
            }
            match tokio::time::timeout(self.timeout, rx).await {
                Ok(Ok(decision)) => decision,
                // Sender dropped without a decision (turn cancelled): fail closed.
                Ok(Err(_)) => Decision::Deny {
                    reason: "the approval prompt was cancelled; the action was refused".to_string(),
                },
                Err(_) => {
                    if let Ok(mut map) = self.pending.lock() {
                        map.remove(&request.call_id);
                    }
                    Decision::Deny {
                        reason: "the approval prompt timed out; the action was refused".to_string(),
                    }
                }
            }
        })
    }
}

/// Classify a tool call into a [`RiskClass`]. Unknown tools default to `Destructive`
/// (ask) — fail safe, never silently allow something we do not understand.
fn classify_call(tool_name: &str, arguments: &str) -> RiskClass {
    match tool_name {
        // Pure, workspace-scoped reads and bookkeeping: never need approval.
        "read" | "grep" | "recall" | "todo" | "activate_skill" | "read_skill_file" => {
            RiskClass::Safe
        }
        // Network read.
        "web_search" => RiskClass::Network,
        // Subagents run their own (already-gated) tool tables; the dispatch itself is safe.
        "subagent" => RiskClass::Safe,
        // The shell is where everything sharp lives: classify the command line.
        "bash" => classify_bash(&extract_command(arguments)),
        // Anything we do not recognise: ask a human.
        _ => RiskClass::Destructive,
    }
}

/// Pull the `command` field out of the tool's JSON arguments without a full JSON parse
/// dependency in the hot path; falls back to the raw string if it is not JSON.
fn extract_command(arguments: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(command) = value.get("command").and_then(|v| v.as_str())
    {
        return command.to_string();
    }
    arguments.to_string()
}

/// Classify a shell command line by its *first meaningful token* and a few substrings.
///
/// This is intentionally heuristic and **not** a security boundary (the sandbox is; see
/// `docs/security.md`). Its job is to decide "safe / ask / refuse" for the human-approval
/// UX, and it fails safe: anything unrecognised is `Destructive` (ask).
fn classify_bash(command: &str) -> RiskClass {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return RiskClass::Safe;
    }

    // Catastrophic patterns: refuse outright regardless of profile.
    let catastrophic = [
        "rm -rf /",
        "rm -rf /*",
        "rm -rf ~",
        ":(){",
        "mkfs",
        "dd if=/dev/zero",
        "> /dev/sda",
        "chmod -r 777 /",
        "chown -r /",
    ];
    let lower = trimmed.to_ascii_lowercase();
    if catastrophic.iter().any(|pat| lower.contains(pat)) {
        return RiskClass::Catastrophic;
    }

    // Destructive verbs / substrings: ask.
    let destructive = [
        "rm ",
        "rmdir",
        "mv ",
        "dd ",
        "truncate",
        "shred",
        "chmod",
        "chown",
        "sudo",
        "su ",
        "kill",
        "pkill",
        "shutdown",
        "reboot",
        "systemctl",
        "launchctl",
        "git push",
        "git reset --hard",
        "git clean",
        "git checkout --",
        "npm publish",
        "pip install",
        "pip3 install",
        "cargo install",
        "brew install",
        "apt install",
        "apt-get install",
        "> /etc",
    ];
    if destructive.iter().any(|pat| lower.contains(pat)) {
        return RiskClass::Destructive;
    }

    // Network verbs: ask (or allow under Full).
    let network = [
        "curl", "wget", "nc ", "ncat", "ssh ", "scp ", "rsync", "sftp", "telnet", "git clone",
        "git fetch", "git pull", "npm install", "pnpm install", "yarn install", "pip download",
    ];
    let first = lower.split_whitespace().next().unwrap_or("");
    if network
        .iter()
        .any(|pat| first == pat.trim() || lower.contains(pat))
    {
        return RiskClass::Network;
    }

    // Known-safe read / build / inspection verbs: allow.
    let safe = [
        "ls", "cat", "head", "tail", "grep", "rg", "find", "wc", "pwd", "cd", "echo", "printf",
        "sed", "awk", "sort", "uniq", "jq", "tree", "which", "env", "date", "stat", "file",
        "diff", "git status", "git log", "git diff", "git show", "git branch", "cargo build",
        "cargo test", "cargo check", "cargo clippy", "cargo fmt", "cargo run", "npm run",
        "npm test", "python", "python3", "node", "make", "go build", "go test",
    ];
    if safe.iter().any(|pat| first == pat.trim() || lower.starts_with(pat)) {
        return RiskClass::Safe;
    }

    // Unknown command: ask.
    RiskClass::Destructive
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_commands_are_safe() {
        assert_eq!(classify_bash("ls -la"), RiskClass::Safe);
        assert_eq!(classify_bash("cargo build"), RiskClass::Safe);
        assert_eq!(classify_bash("git status"), RiskClass::Safe);
        assert_eq!(classify_bash("grep foo ."), RiskClass::Safe);
    }

    #[test]
    fn destructive_commands_ask() {
        assert_eq!(classify_bash("rm -rf target"), RiskClass::Destructive);
        assert_eq!(classify_bash("sudo apt install x"), RiskClass::Destructive);
        assert_eq!(classify_bash("git push origin main"), RiskClass::Destructive);
    }

    #[test]
    fn catastrophic_is_refused_everywhere() {
        assert_eq!(classify_bash("rm -rf /"), RiskClass::Catastrophic);
        assert!(matches!(
            CapabilityProfile::Full.resolve(RiskClass::Catastrophic),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn unknown_commands_ask() {
        assert_eq!(classify_bash("frobnicate --all"), RiskClass::Destructive);
    }

    #[test]
    fn network_commands() {
        assert_eq!(classify_bash("curl https://example.com"), RiskClass::Network);
        assert_eq!(classify_bash("git clone repo"), RiskClass::Network);
    }

    #[test]
    fn profile_resolution() {
        assert!(matches!(
            CapabilityProfile::ReadOnly.resolve(RiskClass::Safe),
            Verdict::Allow
        ));
        assert!(matches!(
            CapabilityProfile::ReadOnly.resolve(RiskClass::Network),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            CapabilityProfile::WorkspaceWrite.resolve(RiskClass::Destructive),
            Verdict::Ask
        ));
        assert!(matches!(
            CapabilityProfile::Full.resolve(RiskClass::Network),
            Verdict::Allow
        ));
    }

    #[tokio::test]
    async fn broker_ask_then_resolve() {
        let broker = ApprovalBroker::with_defaults(None, None);
        let mut rx = broker.subscribe();
        let request = PermissionRequest {
            call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            arguments: r#"{"command":"rm -rf target"}"#.to_string(),
        };
        // classify under WorkspaceWrite → Ask.
        assert!(matches!(broker.classify(&request), Verdict::Ask));

        let broker = std::sync::Arc::new(broker);
        let b2 = std::sync::Arc::clone(&broker);
        let req2 = request.clone();
        let handle = tokio::spawn(async move { b2.approve(&req2).await });

        let prompt = rx.recv().await.expect("prompt");
        assert_eq!(prompt.call_id, "call-1");
        assert!(broker.resolve(&prompt.call_id, Decision::Allow));
        let decision = handle.await.expect("join");
        assert!(matches!(decision, Decision::Allow));
    }

    #[tokio::test]
    async fn broker_no_subscriber_fails_closed() {
        let broker = ApprovalBroker::with_defaults(None, None);
        let request = PermissionRequest {
            call_id: "call-2".to_string(),
            tool_name: "bash".to_string(),
            arguments: r#"{"command":"rm -rf x"}"#.to_string(),
        };
        // No receiver subscribed: approve must deny immediately rather than hang.
        let decision = broker.approve(&request).await;
        assert!(matches!(decision, Decision::Deny { .. }));
    }
}
