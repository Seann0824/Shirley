//! Shared view model for chat transcripts.
//!
//! These types describe **what a rendered conversation is made of** (a message, a tool group,
//! a compaction notice). They live above any single interface: the TUI renders them with ratatui,
//! the desktop/server transports serialise them into wire DTOs, and the shared [`super::session`]
//! orchestration layer builds them from SDK `Message`s. Keeping them here means the shared session
//! layer no longer has to reach up into the TUI module for its data types.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    System,
    Error,
    /// A neutral system notice shown inline (e.g. "images were dropped, continuing"). Distinct
    /// from `System` (command output) and `Error`; rendered muted, no label, not markdown.
    Notice,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ToolCallView {
    pub id: String,
    pub name: String,
    // Raw JSON arguments, kept so the TUI can show which file was read
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ToolGroup {
    pub calls: Vec<ToolCallView>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Message(ChatMessage),
    Tools(ToolGroup),
    Compact { content: String, expanded: bool },
}
