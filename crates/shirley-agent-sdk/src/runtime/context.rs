//! The **additional-context** seam for each request: the SDK provides the hook, the
//! application provides the content.
//!
//! Compaction reduces the conversation to a lossy summary, so the model easily loses
//! track of "where it got to". Applications often need to append a system message at the
//! **end of each request** (e.g. a task ledger / progress checklist / temporary
//! constraints), and that message must **not participate in compaction and must survive
//! across it**.
//!
//! The SDK does not care whether this message is a "task ledger" or anything else — that
//! is the application's business. The SDK only provides a generic seam:
//! [`ContextProvider`]. `Agent` calls it while assembling each request and appends the
//! returned text as a system message **at the end**.
//!
//! Why the end (this relates to KV prefix caching — don't change it):
//! - The additional context does not enter `self.messages`, so compaction can't touch it
//!   and it naturally survives across compaction;
//! - Appending at the tail leaves the prefix untouched, so the prefix cache still hits
//!   while the content is stable;
//! - The content only changes when the provider decides to change it, keeping the change
//!   points minimal.

/// Provider of the additional context appended to the end of each request.
///
/// Implementations must be `Send + Sync` (the stream returned by `run_stream` is `Send`).
/// When `context()` returns `Some(text)`, `Agent` wraps it into a `System` message and
/// appends it to the end of the current request; returning `None` means no injection this
/// turn.
///
/// Providers usually need interior mutability (counters / state) — handle it yourself with
/// `Mutex` / `AtomicUsize` etc., since `context()` takes `&self`.
pub trait ContextProvider: Send + Sync {
    /// Produce the system text to append this turn; `None` means no injection.
    fn context(&self) -> Option<String>;

    /// State snapshot that must be carried into compaction. Defaults to no injection;
    /// providers should return only what is necessary (task state, etc.) to avoid dumping
    /// the full skill/retrieved-memory payload into the summary request again.
    /// This method must not advance the normal request's turn counter.
    fn compaction_context(&self) -> Option<String> {
        None
    }
}
