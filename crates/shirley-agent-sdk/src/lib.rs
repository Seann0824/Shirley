mod adapter;
pub mod error;
mod message;
mod runtime;
pub mod sandbox;
pub mod token;
mod tool;
pub mod workspace;

pub use adapter::{AdapterError, ModelConfig, ModelProtocol, ToolCallingMode};
pub use error::{ErrorKind, SdkError};
#[expect(
    deprecated,
    reason = "re-export the deprecated `Content` alias so 0.0.3 callers keep compiling; removed in 0.0.4"
)]
pub use message::Content;
pub use message::{
    ArtifactId, ContentPart, ImageDetail, ImagePart, ImageSource, Message, MessageContent,
    ModelAccess, ResourceMeta, ResourceRef, ToolCall, ToolOutcome, Usage,
};
pub use runtime::{
    Agent, AgentError, AgentEvent, CompactParts, CompactionConfig, ContextProvider, CutPlan,
    RunResult, StopReason, SystemPrompt, SystemPromptContext, plan_cut,
};
pub use shirley_agent_sdk_macros::tool;
pub use token::{HeuristicCounter, TokenCounter, count_message, count_messages, count_text};
pub use tool::*;
