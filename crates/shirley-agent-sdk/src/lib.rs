mod adapter;
pub mod error;
mod message;
mod runtime;
pub mod sandbox;
pub mod token;
mod tool;
pub mod workspace;

pub use adapter::{AdapterError, ModelConfig, ModelProtocol};
pub use shirley_agent_sdk_macros::tool;
pub use error::{ErrorKind, SdkError};
pub use message::{Message, ToolCall, Usage};
pub use runtime::{
    Agent, AgentError, AgentEvent, CompactParts, CompressionConfig, ContextProvider, CutPlan,
    RunResult, StopReason, SystemPrompt, SystemPromptContext, plan_cut,
};
pub use token::{HeuristicCounter, TokenCounter, count_message, count_messages, count_text};
pub use tool::*;
