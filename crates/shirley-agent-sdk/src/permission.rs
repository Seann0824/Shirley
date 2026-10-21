//! The permission seam: a business-agnostic hook that lets the application decide
//! **whether a tool call may run** before the tool actually executes.
//!
//! Design intent (mirrors `docs/security.md` §5, but kept minimal and dependency-free):
//!
//! - The SDK knows nothing about *what* is dangerous. It only knows how to ask. The
//!   application installs one [`Approver`] into the [`crate::ToolContext`]; if none is
//!   installed, every tool runs exactly as before (fully backwards compatible).
//! - A call is classified into one of three verdicts ([`Verdict`]):
//!   [`Verdict::Allow`] (run it), [`Verdict::Deny`] (refuse, with a reason the model can
//!   read), or [`Verdict::Ask`] (pause and let a human decide via [`Approver::approve`]).
//!   `Ask` is where the "C 档体感" — per-action human approval — comes from.
//! - The gate is **transport-agnostic**: it lives in the runtime (`agent.rs`), so TUI,
//!   desktop (Tauri) and the HTTP server all get the exact same semantics. It must never
//!   be re-implemented in a transport layer.
//!
//! The classification (`Allow` / `Deny` / `Ask`) is a product decision and therefore
//! lives entirely in the application's [`Approver`] implementation. The SDK only owns the
//! *shape* of the conversation and the enforcement point.

use std::pin::Pin;
use std::sync::Arc;

/// What the application wants to do with a tool call, decided **without** side effects.
///
/// This is the three-way policy verdict. `Ask` does not mean "maybe" — it means "I refuse
/// to decide on my own; route this to a human and wait for their [`Decision`]".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run the tool normally.
    Allow,
    /// Refuse outright. The `reason` is surfaced to the model as a tool error so it can
    /// change strategy (and to the UI as an audit line). Not retryable.
    Deny { reason: String },
    /// Pause: ask a human through [`Approver::approve`] and honour their [`Decision`].
    Ask,
}

/// The final outcome of an approval. Unlike [`Verdict`] this is always decisive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The human (or an auto-approval rule) said yes: run the tool.
    Allow,
    /// The human said no, or approval timed out / was cancelled. `reason` goes back to the
    /// model as the tool error text.
    Deny { reason: String },
}

/// A single pending tool call, as seen by the permission layer.
///
/// Deliberately content-carrying but minimal: enough for a policy to classify and for a
/// human to make an informed decision, without leaking the whole conversation.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// The provider-assigned call id (stable across the `ToolStarted` / `ToolFinished`
    /// events, so a UI can correlate an approval prompt with the tool card it belongs to).
    pub call_id: String,
    /// The tool name (`bash`, `web_search`, …).
    pub tool_name: String,
    /// Raw JSON arguments string, exactly as the model produced them.
    pub arguments: String,
}

/// Resolves a [`Verdict::Ask`] into a final [`Decision`], possibly by blocking on a human.
pub type ApprovalFuture<'a> = Pin<Box<dyn Future<Output = Decision> + Send + 'a>>;

/// The application-provided permission authority.
///
/// Implementors decide the policy (`classify`) and, when the policy says "ask", collect a
/// human decision (`approve`). The two are split so a policy can short-circuit obvious
/// allows/denies without ever touching the UI.
///
/// Both methods are `&self` (not `&mut`) because tool calls run concurrently under
/// `FuturesUnordered`; any mutable state must be behind interior mutability (e.g. a
/// channel sender). Implementations are responsible for their own timeout: when `approve`
/// resolves, it must resolve to a concrete [`Decision`] — a hung approval would stall the
/// whole turn.
pub trait Approver: Send + Sync {
    /// Classify the call. Pure and fast: no I/O, no blocking.
    fn classify(&self, request: &PermissionRequest) -> Verdict;

    /// Collect a human decision for an `Ask`. May block (await) — the runtime is waiting.
    fn approve<'a>(&'a self, request: &'a PermissionRequest) -> ApprovalFuture<'a>;
}

/// A concrete wrapper so the application can put an `Arc<dyn Approver>` into the
/// type-erased [`crate::ToolContext`] and the runtime can pull it back out by a known type.
///
/// `ToolContext` is keyed by [`std::any::TypeId`], so a bare `Arc<dyn Approver>` cannot be
/// retrieved by its trait type; this newtype gives it a stable concrete key.
#[derive(Clone)]
pub struct ApproverHandle(pub Arc<dyn Approver>);

impl std::fmt::Debug for ApproverHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApproverHandle(..)")
    }
}
