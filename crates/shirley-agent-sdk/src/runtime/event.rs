use crate::message;
use std::fmt;

#[derive(Debug, Clone)]
pub struct RunResult {
    // Messages added by this call, in the order they were sent
    pub messages: Vec<message::Message>,

    // Why the run stopped
    pub stop_reason: StopReason,

    pub usage: message::Usage,
}

impl RunResult {
    #[must_use]
    pub fn cache_hit_rate(&self) -> Option<f64> {
        self.usage.cache_hit_rate()
    }
}

#[derive(Debug, Clone)]
pub enum StopReason {
    Completed,
    MaxStepsReached,
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Retrying {
        attempt: usize,
        reason: String,
    },
    ContentDelta(String),
    ReasoningDelta(String),
    MessageAdded(message::Message),
    CompactionStarted,
    CompactionFinished,
    ContextUsage {
        used_tokens: u64,
        limit_tokens: u64,
    },
    ToolStarted {
        call_id: String,
        name: String,
        /// Tool-call arguments (raw JSON string), for the UI to show the call intent.
        arguments: String,
    },
    ToolFinished {
        call_id: String,
        name: String,
        /// Whether the tool succeeded (the `Err` branch is `false`).
        ok: bool,
        /// What the tool returned; on failure it is the error text (matching the stored tool message).
        ///
        /// This is **the text the model reads** -- flattened by design, because that is what goes
        /// into the `Tool` message. It is not the shape the UI should render; see `structured`.
        output: String,
        /// The tool's raw return value before flattening (`None` on failure).
        ///
        /// Appended, not replacing `output`: `output` is the model's view, this is the view for
        /// humans (the application layer turns it into render parts; the SDK has no opinion on
        /// what a "part" is -- see `docs/render-parts.md`).
        structured: Option<serde_json::Value>,
        /// Stable error class on failure (`None` on success). Lets consumers classify a
        /// failure structurally instead of matching the display text — see `error.rs`.
        error_kind: Option<crate::ErrorKind>,
        /// Execution time in milliseconds, for the UI to show.
        elapsed_ms: u64,
    },
    // Reported after each model call, to observe cache hits in real time
    Usage(message::Usage),
    /// A neutral, human-readable notice for the user (not an error, not model content).
    ///
    /// Currently used for silent image degradation: when the model cannot read images the
    /// request is resent as text and the user is told so in plain language. Carried as its
    /// own event (not a `ContentDelta`) because deltas are transient view state that the real
    /// assistant message replaces on `MessageAdded`; a notice must survive as its own item.
    Notice(String),
    Finished(RunResult),
}

impl fmt::Display for AgentEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retrying { attempt, reason } => {
                write!(f, "Retrying model request ({attempt}): {reason}")
            }
            Self::ContentDelta(text) | Self::ReasoningDelta(text) | Self::Notice(text) => {
                write!(f, "{text}")
            }
            Self::MessageAdded(message) => write!(f, "{message}"),
            Self::CompactionStarted => write!(f, "compacting context"),
            Self::CompactionFinished => write!(f, "context compaction finished"),
            Self::ContextUsage {
                used_tokens,
                limit_tokens,
            } => {
                let _ = write!(f, "context usage: {used_tokens}/{limit_tokens}");
                Ok(())
            }
            Self::ToolStarted { call_id, name, .. } => {
                let _ = write!(f, "🔧 Tool Started [{call_id}]: {name}");
                Ok(())
            }
            Self::ToolFinished {
                call_id, name, ok, ..
            } => {
                let mark = if *ok { "✅" } else { "❌" };
                let _ = write!(f, "{mark} Tool Finished [{call_id}]: {name}");
                Ok(())
            }
            Self::Usage(usage) => {
                let hit = match usage.cache_hit_rate() {
                    Some(rate) => format!("{:.1}%", rate * 100.0),
                    None => "n/a".to_owned(),
                };
                let _ = write!(
                    f,
                    "📊 Usage: in={} (cached={}, hit={}) out={}",
                    usage.input_tokens,
                    usage.cached_tokens(),
                    hit,
                    usage.output_tokens
                );
                Ok(())
            }
            Self::Finished(result) => {
                let _ = write!(
                    f,
                    "🏁 Finished: {:?} ({} messages)",
                    result.stop_reason,
                    result.messages.len()
                );
                Ok(())
            }
        }
    }
}
