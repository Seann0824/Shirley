use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: Content,
    },
    Assistant {
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(default)]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        // Symmetric with `User`: a tool result may carry text and/or images (e.g. a
        // computer-use screenshot). `Content`'s Serialize keeps a text-only result a plain
        // string, so historical logs round-trip and text-only requests keep the prefix cache.
        content: Option<Content>,
    },
    // Never shown; downgraded to a system message when encoding a request
    ContextSummary {
        content: String,
    },
}

/// Message content: text, or a mix of text and images.
///
/// Backward compatible with the historical "content is a plain string" shape:
/// content that is pure text serializes as a JSON string, and a JSON string
/// deserializes into a single text part. Only content that actually carries an
/// image becomes a JSON array of parts. This keeps old session logs readable and
/// keeps the request bytes for a text-only message identical (KV prefix cache).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Content(Vec<ContentPart>);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image(ImagePart),
}

/// An image reference. The SDK stores a **reference**, never decoded pixels.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ImagePart {
    pub source: ImageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ImageDetail>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    /// A complete URL. Covers both `data:` (base64 inline) and `https:` forms.
    Url { url: String },
    /// A provider-side file reference (e.g. a file uploaded through the Files API).
    FileId { file_id: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Low,
    High,
    Auto,
}

impl Content {
    /// Build a text-only content (the common case).
    pub fn text(text: impl Into<String>) -> Self {
        Content(vec![ContentPart::Text { text: text.into() }])
    }

    #[must_use]
    pub fn parts(parts: Vec<ContentPart>) -> Self {
        Content(parts)
    }

    #[must_use]
    pub fn as_parts(&self) -> &[ContentPart] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// True when every part is text (so it can be encoded as a plain string).
    #[must_use]
    pub fn is_text_only(&self) -> bool {
        self.0
            .iter()
            .all(|part| matches!(part, ContentPart::Text { .. }))
    }

    #[must_use]
    pub fn has_images(&self) -> bool {
        self.0
            .iter()
            .any(|part| matches!(part, ContentPart::Image(_)))
    }

    /// Concatenated text of all text parts; images are ignored.
    /// Used for display, titles and token estimation.
    #[must_use]
    pub fn as_text(&self) -> String {
        let mut out = String::new();
        for part in &self.0 {
            if let ContentPart::Text { text } = part {
                out.push_str(text);
            }
        }
        out
    }
}

impl ImagePart {
    pub fn url(url: impl Into<String>) -> Self {
        ImagePart {
            source: ImageSource::Url { url: url.into() },
            detail: None,
        }
    }

    pub fn file_id(file_id: impl Into<String>) -> Self {
        ImagePart {
            source: ImageSource::FileId {
                file_id: file_id.into(),
            },
            detail: None,
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: ImageDetail) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl ContentPart {
    #[must_use]
    pub fn image(part: ImagePart) -> Self {
        ContentPart::Image(part)
    }
}

impl From<String> for Content {
    fn from(value: String) -> Self {
        Content(vec![ContentPart::Text { text: value }])
    }
}

impl From<&str> for Content {
    fn from(value: &str) -> Self {
        Content(vec![ContentPart::Text {
            text: value.to_owned(),
        }])
    }
}

impl From<Vec<ContentPart>> for Content {
    fn from(value: Vec<ContentPart>) -> Self {
        Content(value)
    }
}

impl PartialEq<str> for Content {
    fn eq(&self, other: &str) -> bool {
        self.as_text() == other
    }
}

impl PartialEq<&str> for Content {
    fn eq(&self, other: &&str) -> bool {
        self.as_text() == *other
    }
}

impl Serialize for Content {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Pure text (including empty) stays a plain string: identical bytes to the
        // historical shape, so old logs round-trip and text-only requests keep the
        // prefix cache.
        if self.is_text_only() {
            return serializer.serialize_str(&self.as_text());
        }
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Text(String),
            Parts(Vec<ContentPart>),
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Text(text) => Content(vec![ContentPart::Text { text }]),
            Repr::Parts(parts) => Content(parts),
        })
    }
}

impl fmt::Display for Content {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for part in &self.0 {
            match part {
                ContentPart::Text { text } => {
                    let _ = write!(f, "{text}");
                }
                ContentPart::Image(_) => {
                    if !first {
                        let _ = write!(f, " ");
                    }
                    let _ = write!(f, "[image]");
                }
            }
            first = false;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

// Tracks how much input, how much output, how much cache hit, and how many reasoning tokens were spent.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    // None means the provider did not report it, which differs from reporting 0
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    // The cumulative hit rate counts only calls that reported cache data; when older data
    // lacks this field, fall back to the single-call input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_reported_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

impl Usage {
    fn reported_input_tokens(&self) -> Option<u64> {
        self.cache_reported_input_tokens
            .or_else(|| self.cached_input_tokens.map(|_| self.input_tokens))
    }

    #[must_use]
    pub fn cached_tokens(&self) -> u64 {
        self.cached_input_tokens.unwrap_or(0)
    }

    #[must_use]
    pub fn missed_input_tokens(&self) -> u64 {
        self.input_tokens.saturating_sub(self.cached_tokens())
    }

    // Returns None when unreported; don't treat "can't measure" as a 0% hit rate
    #[must_use]
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let reported_input = self.reported_input_tokens()?;
        if reported_input == 0 {
            return None;
        }
        self.cached_input_tokens
            .map(|cached| cached as f64 / reported_input as f64)
    }
}

impl std::ops::Add for Usage {
    type Output = Usage;

    fn add(self, rhs: Usage) -> Usage {
        let reported_input = match (self.reported_input_tokens(), rhs.reported_input_tokens()) {
            (Some(a), Some(b)) => Some(a + b),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        Usage {
            input_tokens: self.input_tokens + rhs.input_tokens,
            output_tokens: self.output_tokens + rhs.output_tokens,
            cache_reported_input_tokens: reported_input,
            // If one side is unreported, keep the known side; don't accumulate it as 0
            cached_input_tokens: match (self.cached_input_tokens, rhs.cached_input_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
            reasoning_tokens: match (self.reasoning_tokens, rhs.reasoning_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
        }
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::User { content } => {
                let _ = write!(f, "👤 User: {content}");
                Ok(())
            }
            Message::Assistant {
                content,
                reasoning_content,
                tool_calls,
            } => {
                if let Some(reasoning) = reasoning_content {
                    writeln!(f, "\u{1f9e0} Thinking: {reasoning}")?;
                }
                if let Some(content) = content {
                    writeln!(f, "🤖 Assistant: {content}")?;
                }

                for call in tool_calls {
                    writeln!(f, "🔧 Tool Call: {}({})", call.name, call.arguments)?;
                }

                Ok(())
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                let _ = write!(
                    f,
                    "⚙️ Tool Result [{}]: {}",
                    tool_call_id,
                    content
                        .as_ref()
                        .map(std::string::ToString::to_string)
                        .unwrap_or_default()
                );
                Ok(())
            }
            Message::System { content } => {
                let _ = write!(f, "🖥️ System: {content}");
                Ok(())
            }
            Message::ContextSummary { content } => {
                let _ = write!(f, "🖥️ Context Summary: {content}");
                Ok(())
            }
        }
    }
}
