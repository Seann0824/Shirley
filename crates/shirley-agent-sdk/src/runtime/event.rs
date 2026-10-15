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
    ContentDelta(String),
    ReasoningDelta(String),
    MessageAdded(message::Message),
    CompressionStarted,
    CompressionFinished,
    ContextUsage { used_tokens: u64, limit_tokens: u64 },
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
        output: String,
        /// Stable error class on failure (`None` on success). Lets consumers classify a
        /// failure structurally instead of matching the display text — see `error.rs`.
        error_kind: Option<crate::ErrorKind>,
        /// Execution time in milliseconds, for the UI to show.
        elapsed_ms: u64,
    },
    // Reported after each model call, to observe cache hits in real time
    Usage(message::Usage),
    Finished(RunResult),
}

impl fmt::Display for AgentEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContentDelta(text) | Self::ReasoningDelta(text) => write!(f, "{text}"),
            Self::MessageAdded(message) => write!(f, "{message}"),
            Self::CompressionStarted => write!(f, "compacting context"),
            Self::CompressionFinished => write!(f, "context compaction finished"),
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
            Self::ToolFinished { call_id, name, ok, .. } => {
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
