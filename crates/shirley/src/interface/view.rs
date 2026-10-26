//! Shared view model for chat transcripts.
//!
//! These types describe **what a rendered conversation is made of** (a message, a tool group,
//! a compaction notice). They live above any single interface: the TUI renders them with ratatui,
//! the desktop/server transports serialise them into wire DTOs, and the shared [`super::session`]
//! orchestration layer builds them from SDK `Message`s. Keeping them here means the shared session
//! layer no longer has to reach up into the TUI module for its data types.
//!
//! **Two views, not one** (`docs/render-parts.md`): the SDK's `Message` is the *model* view -- what
//! gets sent to the provider. [`RenderPart`] is the *render* view -- what a human sees. They
//! coincide only for plain text; a tool returning an image, a chart or a file has no place in
//! `Message`, so the render view is where those live. Every part must be able to degrade to text
//! ([`RenderPart::fallback_text`]), so an interface that does not understand a medium shows
//! something readable instead of a blank.

use serde::{Deserialize, Serialize};
use shirley_agent_sdk::MessageContent;

/// Where a part's bytes come from. **References only -- never pixels.**
///
/// Keeping the bytes out of the event stream and the session log is what lets the same payload
/// travel over three transports (Tauri IPC, HTTP, and the plain-text interfaces) without any of
/// them inheriting a base64 blob (`docs/render-parts.md` P1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssetSource {
    /// An artifact owned by this workspace, served out of band by the application.
    Artifact {
        /// Opaque artifact id (content hash). The store resolves it, never the webview.
        id: String,
    },
    /// An external address (`https:` / `data:`). Allowed, but the transport caps its length.
    Url {
        /// The URL verbatim.
        url: String,
    },
}

/// One renderable unit of a message or a tool result.
///
/// The wire form is `#[serde(tag = "kind")]`, and the frontend dispatches on `kind` -- that is the
/// whole extension seam: a new medium is a new variant plus a component, with no change to
/// `Message`, to the session log, or to any existing renderer. Consumers must **ignore unknown
/// kinds** rather than fail, so an older frontend can read a newer payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderPart {
    /// Plain text (markdown). The default, and the only kind every interface understands.
    Text {
        /// The markdown source.
        text: String,
    },
    /// An image reference.
    Image {
        /// Where the bytes live.
        source: AssetSource,
        /// Alt text / filename, shown when the bytes cannot be loaded and used as the fallback.
        alt: String,
        /// MIME type, when known (`image/png`, ...).
        #[serde(default)]
        mime: Option<String>,
    },
    /// A file the user can open or download.
    ///
    /// Anything that is not a picture lands here -- a PDF, a deck, a spreadsheet, a clip, a zip --
    /// and the **mime** (not the kind) decides which renderer the interface picks. That is the
    /// whole point of keeping `File` generic: supporting `pptx` next month must not need a new
    /// `RenderPart` variant, a new wire field or a new session code path.
    File {
        /// Where the bytes live. `None` only for a part that merely *mentions* a path (a tool
        /// describing an output it did not store); such a part degrades to its fallback text.
        #[serde(default)]
        source: Option<AssetSource>,
        /// Workspace-relative path, for display. Not a fetchable address -- never build a URL
        /// from it, the webview has no way to read the filesystem.
        #[serde(default)]
        path: String,
        /// Display name.
        name: String,
        /// MIME type, when known. Drives the renderer choice.
        mime: Option<String>,
        /// Size in bytes, when known.
        bytes: Option<u64>,
    },
    /// An external link.
    Link {
        /// Target URL.
        url: String,
        /// Display title.
        #[serde(default)]
        title: String,
    },
    /// Structured data (`schema` picks the renderer). **Data only, never markup or script**:
    /// a remote tool naming a schema is not a rendering grant -- the frontend renders it only if
    /// that schema is registered locally, otherwise it degrades to a JSON disclosure.
    Data {
        /// Renderer key chosen by the producer.
        schema: String,
        /// The payload.
        value: serde_json::Value,
    },
}

impl RenderPart {
    /// A text part.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// True for [`RenderPart::Text`] (the part a text-only interface already knows how to draw).
    #[must_use]
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text { .. })
    }

    /// The part rendered as plain text.
    ///
    /// **This is the contract's hard requirement**: whatever the medium, a text-only consumer (the
    /// TUI, a cloud web client without a renderer, the session log, a model request) must get
    /// something readable. Never empty, never an error.
    #[must_use]
    pub fn fallback_text(&self) -> String {
        match self {
            Self::Text { text } => text.clone(),
            Self::Image { alt, mime, .. } => match mime {
                Some(mime) => format!("[image: {alt} ({mime})]"),
                None => format!("[image: {alt}]"),
            },
            Self::File { name, bytes, .. } => match bytes {
                Some(bytes) => format!("[file: {name}, {bytes} bytes]"),
                None => format!("[file: {name}]"),
            },
            Self::Link { url, title } => {
                if title.is_empty() {
                    format!("[link: {url}]")
                } else {
                    format!("[link: {title} <{url}>]")
                }
            }
            Self::Data { schema, .. } => format!("[data: {schema}]"),
        }
    }
}

/// Render parts joined into plain text (non-text parts become their fallback line).
///
/// Used by interfaces that do not render media: the transcript never goes blank because a message
/// happened to be an image.
#[must_use]
pub fn parts_fallback_text(parts: &[RenderPart]) -> String {
    parts
        .iter()
        .map(RenderPart::fallback_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract render parts from a tool's raw return value.
///
/// The **single convention** (`docs/render-parts.md` 4.1): a tool opts into rich rendering by
/// returning an object with a `parts` array whose elements are [`RenderPart`] shapes. There is no
/// "if the tool is called X, special-case it" branch here -- name-based enrichment is decoration
/// (title/icon), not capability.
///
/// Unknown / malformed entries are skipped (forward compatibility). An empty result means "the
/// caller should fall back to the flattened text", which is what every existing tool gets.
#[must_use]
pub fn parts_from_tool_value(value: Option<&serde_json::Value>) -> Vec<RenderPart> {
    let Some(parts) = value
        .and_then(|value| value.get("parts"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|part| serde_json::from_value::<RenderPart>(part.clone()).ok())
        .collect()
}

/// Project a model-facing message body into render parts.
///
/// This is the fix for "stored but not renderable": the SDK's `MessageContent` already carries
/// images (`ContentPart::Image`), but projecting it with `as_text()` dropped them, so a pasted
/// image was persisted and then silently vanished from the transcript on restore.
#[must_use]
pub fn parts_from_message_content(content: &MessageContent) -> Vec<RenderPart> {
    use shirley_agent_sdk::ContentPart;

    let mut parts = Vec::new();
    for part in content.as_parts() {
        match part {
            ContentPart::Text { text } => {
                if !text.is_empty() {
                    parts.push(RenderPart::Text { text: text.clone() });
                }
            }
            ContentPart::Image(image) => {
                let source = match &image.source {
                    shirley_agent_sdk::ImageSource::Url { url } => {
                        AssetSource::Url { url: url.clone() }
                    }
                    shirley_agent_sdk::ImageSource::FileId { file_id } => AssetSource::Artifact {
                        id: file_id.clone(),
                    },
                };
                parts.push(RenderPart::Image {
                    source,
                    alt: "attached image".to_owned(),
                    mime: None,
                });
            }
        }
    }
    parts
}

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
    /// Render parts for this message (empty = the interface renders `content` as-is).
    ///
    /// `content` stays the plain-text projection (`MessageContent::as_text`), so every consumer
    /// that only knows about text keeps working unchanged.
    pub parts: Vec<RenderPart>,
}

#[derive(Debug, Clone)]
pub struct ToolCallView {
    pub id: String,
    pub name: String,
    // Raw JSON arguments, kept so the TUI can show which file was read
    pub arguments: String,
    /// The tool's flattened output (what the model saw). `None` until the call completes.
    ///
    /// Recorded in the view so switching sessions / rebuilding from the session log no longer
    /// loses tool results -- previously they only existed in the live frontend state.
    pub output: Option<String>,
    /// Whether the call succeeded (`None` = not finished yet, or restored from a log without
    /// an explicit status).
    pub ok: Option<bool>,
    /// Rich render parts produced from the tool's structured result (empty = render `output`).
    pub parts: Vec<RenderPart>,
}

impl ToolCallView {
    /// A call that has just started (no result yet).
    #[must_use]
    pub fn started(id: String, name: String, arguments: String) -> Self {
        Self {
            id,
            name,
            arguments,
            output: None,
            ok: None,
            parts: Vec::new(),
        }
    }

    /// Record the result of this call.
    pub fn finish(&mut self, ok: Option<bool>, output: String, parts: Vec<RenderPart>) {
        self.output = Some(output);
        self.ok = ok.or(self.ok);
        if !parts.is_empty() {
            self.parts = parts;
        }
    }

    /// The text a text-only interface should show for this call's result.
    #[must_use]
    pub fn result_text(&self) -> Option<String> {
        if !self.parts.is_empty() {
            return Some(parts_fallback_text(&self.parts));
        }
        self.output.clone()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_part_degrades_to_text() {
        let parts = vec![
            RenderPart::Text {
                text: "hello".into(),
            },
            RenderPart::Image {
                source: AssetSource::Artifact {
                    id: "sha256:abc".into(),
                },
                alt: "chart.png".into(),
                mime: Some("image/png".into()),
            },
            RenderPart::File {
                source: None,
                path: "out/report.pdf".into(),
                name: "report.pdf".into(),
                mime: None,
                bytes: Some(2048),
            },
            RenderPart::Link {
                url: "https://example.com".into(),
                title: "Example".into(),
            },
            RenderPart::Data {
                schema: "table".into(),
                value: serde_json::json!({ "rows": 2 }),
            },
        ];
        let text = parts_fallback_text(&parts);
        assert!(text.contains("hello"), "{text}");
        assert!(text.contains("chart.png"), "{text}");
        assert!(text.contains("report.pdf"), "{text}");
        assert!(text.contains("https://example.com"), "{text}");
        assert!(text.contains("table"), "{text}");
        // No part may degrade to an empty line: that is exactly the "blank transcript" bug.
        for part in &parts {
            assert!(!part.fallback_text().is_empty(), "{part:?}");
        }
    }

    #[test]
    fn tool_value_parts_are_parsed_by_kind() {
        let value = serde_json::json!({
            "parts": [
                { "kind": "text", "text": "done" },
                { "kind": "image", "source": { "kind": "artifact", "id": "abc" }, "alt": "a.png" },
                { "kind": "hologram", "whatever": 1 }
            ]
        });
        let parts = parts_from_tool_value(Some(&value));
        // The unknown kind is skipped, not fatal -- an older app reading a newer payload.
        assert_eq!(parts.len(), 2);
        assert!(parts[0].is_text());
        assert!(
            matches!(&parts[1], RenderPart::Image { source: AssetSource::Artifact { id }, .. } if id == "abc")
        );
    }

    #[test]
    fn tool_value_without_parts_falls_back_to_text() {
        let value = serde_json::json!({ "result": "ok" });
        assert!(parts_from_tool_value(Some(&value)).is_empty());
        assert!(parts_from_tool_value(None).is_empty());
    }

    #[test]
    fn message_content_projects_images_instead_of_dropping_them() {
        use shirley_agent_sdk::{ContentPart, ImagePart, MessageContent};
        let content = MessageContent::parts(vec![
            ContentPart::Text {
                text: "what is this?".into(),
            },
            ContentPart::Image(ImagePart::url("data:image/png;base64,AAAA")),
        ]);
        let parts = parts_from_message_content(&content);
        assert_eq!(
            parts.len(),
            2,
            "the image must survive projection: {parts:?}"
        );
        assert!(parts[0].is_text());
        assert!(
            matches!(&parts[1], RenderPart::Image { source: AssetSource::Url { url }, .. } if url.starts_with("data:image/png"))
        );
    }

    #[test]
    fn finishing_a_call_records_output_and_parts() {
        let mut call = ToolCallView::started("c1".into(), "read".into(), "{}".into());
        assert_eq!(call.result_text(), None);
        call.finish(Some(true), "plain text".into(), Vec::new());
        assert_eq!(call.result_text().as_deref(), Some("plain text"));
        call.finish(
            Some(true),
            "{\"parts\":[]}".into(),
            vec![RenderPart::Image {
                source: AssetSource::Artifact { id: "x".into() },
                alt: "x.png".into(),
                mime: None,
            }],
        );
        // Parts win over the flattened text when both exist.
        assert_eq!(call.result_text().as_deref(), Some("[image: x.png]"));
    }
}
