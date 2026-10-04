use crate::message;
use std::fmt;

#[derive(Debug)]
pub struct RunResult {
    // 本次调用新增的消息，按照发送顺序排序
    pub messages: Vec<message::Message>,

    // 停止的原因
    pub stop_reason: StopReason,

    pub usage: message::Usage,
}

impl RunResult {
    pub fn cache_hit_rate(&self) -> Option<f64> {
        self.usage.cache_hit_rate()
    }
}

#[derive(Debug)]
pub enum StopReason {
    Completed,
    MaxStepsReached,
    Cancelled,
}

#[derive(Debug)]
pub enum AgentEvent {
    ContentDelta(String),
    ReasoningDelta(String),
    MessageAdded(message::Message),
    CompressionStarted,
    CompressionFinished,
    ContextUsage { used_tokens: u64, limit_tokens: u64 },
    ToolStarted { call_id: String, name: String },
    ToolFinished { call_id: String, name: String },
    // 每次模型调用后上报，便于实时观察缓存命中
    Usage(message::Usage),
    Finished(RunResult),
}

impl fmt::Display for AgentEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContentDelta(text) | Self::ReasoningDelta(text) => write!(f, "{text}"),
            Self::MessageAdded(message) => write!(f, "{message}"),
            Self::CompressionStarted => write!(f, "正在压缩上下文"),
            Self::CompressionFinished => write!(f, "上下文压缩完成"),
            Self::ContextUsage {
                used_tokens,
                limit_tokens,
            } => {
                let _ = write!(f, "上下文用量: {used_tokens}/{limit_tokens}");
                Ok(())
            }
            Self::ToolStarted { call_id, name } => {
                let _ = write!(f, "🔧 Tool Started [{call_id}]: {name}");
                Ok(())
            }
            Self::ToolFinished { call_id, name } => {
                let _ = write!(f, "✅ Tool Finished [{call_id}]: {name}");
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
