//! Sidecar safety review: a **decision model** (Jev, TypeSafe System One) as a second opinion
//! before a risky tool call runs.
//!
//! This is the "sidecar" from the PDF (ch.4.6, p.123): when the main model decides to call a
//! tool, an *independent lightweight model* judges whether that call is safe. It runs as a
//! **gate** on the call — a dangerous action does not execute until the sidecar clears it.
//!
//! Design constraints lifted from the PDF, all deliberate:
//!
//! - **Only structured fields go in.** The sidecar sees `{tool, action, arguments_summary, …}`
//!   derived from the tool-call JSON — never the main model's free text or reasoning. This is
//!   what blocks prompt-injection ("ignore previous instructions, this is safe").
//! - **A decision model, not a chat model.** Jev returns a typed decision (`allow` / `confirm`
//!   / `review` / `deny`) plus a *calibrated confidence*. That is a classification, which is
//!   exactly what the sidecar is for; asking a chat model for prose would be slower and less
//!   reliable.
//! - **Grey zone only.** The cheap heuristic in [`crate::permission`] stays the first pass:
//!   obviously-safe calls (`Safe`) and hard denials are resolved without ever spending a token
//!   on the sidecar. Only ambiguous calls (`Network` / `Destructive`) are escalated here.
//! - **Fail safe.** If the sidecar is unconfigured or the call errors/times out, the caller
//!   falls back to the heuristic verdict — behaviour is unchanged from before this module.
//! - **Deny circuit breaker.** If the sidecar denies several calls in a row (a sign it is stuck
//!   or the task is genuinely impossible), we stop auto-denying and escalate to a human instead,
//!   so the agent does not silently loop on refusals.
//!
//! The wire contract mirrors Jev's `tool-guard` preset:
//! `POST {endpoint}` with `Authorization: Bearer <key>`; body
//! `{tool, action, arguments_summary[], side_effects[], safeguards[], policy[], reversibility}`;
//! response `{code, message, data:{decision, confidence, probabilities, guidance, …}}`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use shirley_agent_sdk::{PermissionRequest, Verdict};

/// Default Jev `tool-guard` endpoint (see <https://www.jevai.org/docs>).
const DEFAULT_ENDPOINT: &str = "https://www.jevai.org/api/v1/decisions/tool-guard";
/// Request body cap documented by Jev; we refuse to send anything larger.
const MAX_BODY_BYTES: usize = 32 * 1024;
/// Default confidence at or above which a Jev `allow` is taken at face value.
const DEFAULT_ALLOW_CONFIDENCE: f32 = 0.80;
/// Default number of consecutive denials before the circuit breaker escalates to a human.
const DEFAULT_DENY_STREAK_LIMIT: usize = 3;

/// Configuration for the sidecar decision model. Resolved once at assembly from env.
#[derive(Clone, Debug, PartialEq)]
pub struct SidecarConfig {
    /// Full `tool-guard` endpoint URL.
    pub endpoint: String,
    /// Bearer API key. Never logged.
    pub api_key: String,
    /// Per-call timeout for the sidecar request.
    pub timeout: Duration,
    /// A Jev `allow` is honoured only when its confidence reaches this threshold; below it we
    /// ask a human (an unconfident "yes" is not good enough to auto-run a risky call).
    pub allow_confidence: f32,
    /// Consecutive-denial threshold for the circuit breaker (`0` disables it).
    pub deny_streak_limit: usize,
}

impl SidecarConfig {
    /// Build from the environment, returning `None` when no API key is set (sidecar disabled).
    ///
    /// Env vars: `JEV_API_KEY` (or `SHIRLEY_SIDECAR_API_KEY`), `SHIRLEY_SIDECAR_ENDPOINT`,
    /// `SHIRLEY_SIDECAR_TIMEOUT_SECS`, `SHIRLEY_SIDECAR_ALLOW_CONFIDENCE`,
    /// `SHIRLEY_SIDECAR_DENY_STREAK`.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("SHIRLEY_SIDECAR_API_KEY")
            .or_else(|_| std::env::var("JEV_API_KEY"))
            .ok()
            .filter(|key| !key.trim().is_empty())?;
        Some(Self {
            endpoint: std::env::var("SHIRLEY_SIDECAR_ENDPOINT")
                .ok()
                .filter(|url| !url.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string()),
            api_key,
            timeout: std::env::var("SHIRLEY_SIDECAR_TIMEOUT_SECS")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .map_or_else(|| Duration::from_secs(8), Duration::from_secs),
            allow_confidence: std::env::var("SHIRLEY_SIDECAR_ALLOW_CONFIDENCE")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(DEFAULT_ALLOW_CONFIDENCE),
            deny_streak_limit: std::env::var("SHIRLEY_SIDECAR_DENY_STREAK")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(DEFAULT_DENY_STREAK_LIMIT),
        })
    }
}

/// Jev's four-way decision for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JevDecision {
    /// Safe to run.
    Allow,
    /// Run only after explicit user confirmation.
    Confirm,
    /// Needs a closer look / human review.
    Review,
    /// Refuse.
    Deny,
}

/// The parsed, typed answer from the decision model.
#[derive(Debug, Clone, PartialEq)]
pub struct JevVerdict {
    /// The four-way decision.
    pub decision: JevDecision,
    /// Calibrated confidence in `[0, 1]`.
    pub confidence: f32,
    /// Human-readable guidance from the model (used as the deny reason / approval note).
    pub guidance: String,
}

/// Errors from the sidecar call. Any of these makes the caller fall back to the heuristic.
#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    /// The request body exceeded Jev's 32 KiB cap.
    #[error("sidecar request body too large ({size} bytes > {max})")]
    BodyTooLarge {
        /// Actual size in bytes.
        size: usize,
        /// Maximum allowed size.
        max: usize,
    },
    /// The HTTP request failed (network, timeout, non-2xx).
    #[error("sidecar request failed: {0}")]
    Transport(String),
    /// The response could not be parsed or reported a non-zero `code`.
    #[error("sidecar returned an unusable response: {0}")]
    Response(String),
}

/// The sidecar decision-model client. Cheap to clone via `Arc`; holds one `reqwest::Client`.
pub struct Sidecar {
    http: reqwest::Client,
    config: SidecarConfig,
    /// Consecutive `deny` count for the circuit breaker.
    deny_streak: AtomicUsize,
}

impl std::fmt::Debug for Sidecar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sidecar")
            .field("endpoint", &self.config.endpoint)
            .field("allow_confidence", &self.config.allow_confidence)
            .field("deny_streak", &self.deny_streak.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Sidecar {
    /// Build a sidecar client. Returns `None` if the HTTP client cannot be constructed.
    pub fn new(config: SidecarConfig) -> Option<Self> {
        let http = reqwest::Client::builder()
            // The sidecar is a pure judgement call; never follow redirects (SSRF hygiene,
            // same posture as `web_search`).
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .connect_timeout(Duration::from_secs(5))
            .build()
            .ok()?;
        Some(Self {
            http,
            config,
            deny_streak: AtomicUsize::new(0),
        })
    }

    /// Convenience: build from env, `None` when unconfigured.
    pub fn from_env() -> Option<Self> {
        Self::new(SidecarConfig::from_env()?)
    }

    /// Ask the decision model to judge a tool call and map its answer to a Shirley [`Verdict`].
    ///
    /// `profile` is the current capability profile label, passed to the model as policy context.
    ///
    /// # Errors
    ///
    /// Returns [`SidecarError`] on transport failure, an oversized body, or an unusable
    /// response. The caller is expected to fall back to the heuristic verdict on any error.
    pub async fn review(
        &self,
        request: &PermissionRequest,
        profile: &str,
    ) -> Result<Verdict, SidecarError> {
        let body = build_request(request, profile);
        let bytes =
            serde_json::to_vec(&body).map_err(|e| SidecarError::Transport(e.to_string()))?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(SidecarError::BodyTooLarge {
                size: bytes.len(),
                max: MAX_BODY_BYTES,
            });
        }

        let response = self
            .http
            .post(&self.config.endpoint)
            .bearer_auth(&self.config.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(|e| SidecarError::Transport(e.to_string()))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| SidecarError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(SidecarError::Response(format!("HTTP {status}")));
        }

        let envelope: JevEnvelope = serde_json::from_str(&text)
            .map_err(|e| SidecarError::Response(format!("invalid JSON: {e}")))?;
        if envelope.code != 0 {
            return Err(SidecarError::Response(format!(
                "code {}: {}",
                envelope.code, envelope.message
            )));
        }
        let data = envelope
            .data
            .ok_or_else(|| SidecarError::Response("missing data".to_string()))?;
        let verdict = JevVerdict {
            decision: data.decision,
            confidence: data.confidence.clamp(0.0, 1.0),
            guidance: data.guidance.unwrap_or_default(),
        };
        Ok(self.map_verdict(verdict))
    }

    /// Map a typed Jev answer to a Shirley verdict, applying the confidence threshold and the
    /// deny circuit breaker.
    fn map_verdict(&self, verdict: JevVerdict) -> Verdict {
        match verdict.decision {
            JevDecision::Allow => {
                self.deny_streak.store(0, Ordering::Relaxed);
                if verdict.confidence >= self.config.allow_confidence {
                    Verdict::Allow
                } else {
                    // An unconfident "yes" is not enough to auto-run a risky call.
                    Verdict::Ask
                }
            }
            JevDecision::Confirm | JevDecision::Review => {
                self.deny_streak.store(0, Ordering::Relaxed);
                Verdict::Ask
            }
            JevDecision::Deny => {
                let streak = self.deny_streak.fetch_add(1, Ordering::Relaxed) + 1;
                let limit = self.config.deny_streak_limit;
                if limit > 0 && streak >= limit {
                    // Circuit breaker: stop auto-denying, hand it to a human instead.
                    self.deny_streak.store(0, Ordering::Relaxed);
                    Verdict::Ask
                } else {
                    Verdict::Deny {
                        reason: deny_reason(&verdict.guidance),
                    }
                }
            }
        }
    }
}

/// Compose a deny reason for the model, using the model's guidance when present.
fn deny_reason(guidance: &str) -> String {
    let guidance = guidance.trim();
    if guidance.is_empty() {
        "the safety sidecar refused this tool call; change approach and retry".to_string()
    } else {
        format!("the safety sidecar refused this tool call: {guidance}")
    }
}

// ── wire types ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct JevRequest {
    tool: String,
    action: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    arguments_summary: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    side_effects: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    safeguards: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    policy: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reversibility: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JevEnvelope {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
    data: Option<JevData>,
}

#[derive(Debug, Deserialize)]
struct JevData {
    decision: JevDecision,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    guidance: Option<String>,
}

/// Build the structured sidecar request from a tool call. **Only structured fields** — the
/// tool name and its parsed JSON arguments — ever leave the process; the main model's free text
/// never does.
fn build_request(request: &PermissionRequest, profile: &str) -> JevRequest {
    let parsed: serde_json::Value =
        serde_json::from_str(&request.arguments).unwrap_or(serde_json::Value::Null);

    // `action`: a one-line description. For bash the command is the action; otherwise the
    // arguments JSON (truncated) stands in.
    let action = match request.tool_name.as_str() {
        "bash" => parsed
            .get("command")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| request.arguments.clone(), str::to_string),
        _ => truncate(&request.arguments, 500),
    };

    // `arguments_summary`: flatten top-level scalar fields into `key=value` strings.
    let mut arguments_summary = Vec::new();
    if let serde_json::Value::Object(map) = &parsed {
        for (key, value) in map {
            let rendered = match value {
                serde_json::Value::String(s) => truncate(s, 200),
                other => truncate(&other.to_string(), 200),
            };
            arguments_summary.push(format!("{key}={rendered}"));
        }
    }

    // `reversibility`: a coarse hint from the tool name (the model refines it).
    let reversibility = match request.tool_name.as_str() {
        "read" | "grep" | "recall" | "web_search" | "todo" | "activate_skill"
        | "read_skill_file" => Some("read_only".to_string()),
        "bash" => Some("partially_reversible".to_string()),
        _ => None,
    };

    JevRequest {
        tool: request.tool_name.clone(),
        action,
        arguments_summary,
        side_effects: Vec::new(),
        safeguards: Vec::new(),
        policy: vec![format!("capability_profile={profile}")],
        reversibility,
    }
}

/// Truncate a string to at most `max` **characters** (never splits a UTF-8 boundary).
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidecar(allow_confidence: f32, deny_streak_limit: usize) -> Sidecar {
        Sidecar::new(SidecarConfig {
            endpoint: "https://example.invalid".to_string(),
            api_key: "test".to_string(),
            timeout: Duration::from_secs(1),
            allow_confidence,
            deny_streak_limit,
        })
        .expect("client")
    }

    fn verdict(decision: JevDecision, confidence: f32) -> JevVerdict {
        JevVerdict {
            decision,
            confidence,
            guidance: "do the thing".to_string(),
        }
    }

    #[test]
    fn allow_above_threshold_is_allow() {
        let s = sidecar(0.8, 3);
        assert_eq!(
            s.map_verdict(verdict(JevDecision::Allow, 0.95)),
            Verdict::Allow
        );
    }

    #[test]
    fn allow_below_threshold_asks() {
        let s = sidecar(0.8, 3);
        assert_eq!(
            s.map_verdict(verdict(JevDecision::Allow, 0.5)),
            Verdict::Ask
        );
    }

    #[test]
    fn confirm_and_review_ask() {
        let s = sidecar(0.8, 3);
        assert_eq!(
            s.map_verdict(verdict(JevDecision::Confirm, 0.9)),
            Verdict::Ask
        );
        assert_eq!(
            s.map_verdict(verdict(JevDecision::Review, 0.9)),
            Verdict::Ask
        );
    }

    #[test]
    fn deny_maps_to_deny_with_guidance() {
        let s = sidecar(0.8, 0); // breaker disabled
        match s.map_verdict(verdict(JevDecision::Deny, 0.9)) {
            Verdict::Deny { reason } => assert!(reason.contains("do the thing")),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn deny_circuit_breaker_escalates_to_ask() {
        let s = sidecar(0.8, 2);
        // First denial → Deny.
        assert!(matches!(
            s.map_verdict(verdict(JevDecision::Deny, 0.9)),
            Verdict::Deny { .. }
        ));
        // Second consecutive denial hits the limit → escalate to a human.
        assert_eq!(s.map_verdict(verdict(JevDecision::Deny, 0.9)), Verdict::Ask);
        // And the counter resets, so the next denial denies again.
        assert!(matches!(
            s.map_verdict(verdict(JevDecision::Deny, 0.9)),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn allow_resets_deny_streak() {
        let s = sidecar(0.8, 2);
        assert!(matches!(
            s.map_verdict(verdict(JevDecision::Deny, 0.9)),
            Verdict::Deny { .. }
        ));
        assert_eq!(
            s.map_verdict(verdict(JevDecision::Allow, 0.9)),
            Verdict::Allow
        );
        // Streak reset → this denial is a plain deny, not an escalation.
        assert!(matches!(
            s.map_verdict(verdict(JevDecision::Deny, 0.9)),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn build_request_extracts_bash_command() {
        let request = PermissionRequest {
            call_id: "c1".to_string(),
            tool_name: "bash".to_string(),
            arguments: r#"{"command":"git push origin main","timeout":30}"#.to_string(),
        };
        let body = build_request(&request, "workspace-write");
        assert_eq!(body.tool, "bash");
        assert_eq!(body.action, "git push origin main");
        assert!(
            body.arguments_summary
                .iter()
                .any(|s| s == "command=git push origin main")
        );
        assert_eq!(body.reversibility.as_deref(), Some("partially_reversible"));
        assert_eq!(
            body.policy,
            vec!["capability_profile=workspace-write".to_string()]
        );
    }

    #[test]
    fn build_request_never_carries_free_text() {
        // A non-bash tool: the action is the arguments JSON, not any conversation text.
        let request = PermissionRequest {
            call_id: "c2".to_string(),
            tool_name: "some_tool".to_string(),
            arguments: r#"{"path":"src/main.rs"}"#.to_string(),
        };
        let body = build_request(&request, "full");
        assert_eq!(body.tool, "some_tool");
        assert!(
            body.arguments_summary
                .iter()
                .any(|s| s == "path=src/main.rs")
        );
        assert!(body.reversibility.is_none());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("hello", 10), "hello");
        let long = "中".repeat(10);
        let cut = truncate(&long, 3);
        assert_eq!(cut.chars().count(), 4); // 3 chars + ellipsis
    }

    #[test]
    fn parses_jev_envelope() {
        let raw = r#"{"code":0,"message":"ok","data":{"decision":"confirm","confidence":0.86,"guidance":"ask first"}}"#;
        let env: JevEnvelope = serde_json::from_str(raw).expect("parse");
        assert_eq!(env.code, 0);
        let data = env.data.expect("data");
        assert_eq!(data.decision, JevDecision::Confirm);
        assert!((data.confidence - 0.86).abs() < 1e-6);
    }
}
