//! The `media` tool: the model-side door into the media library.
//!
//! Why this exists: the bytes of a picture, a PDF or a deck never travel through the
//! conversation. The harness stores them once -- in the same jail the `RenderPart`s already point
//! at -- and a message carries only a handle. This tool is how the model **decides** what to do
//! with a resource: see what is there, ask what one is, or read it.
//!
//! "Read" means something different per medium, and the tool says so honestly instead of
//! pretending:
//!
//! - text-like  -> the text itself, bounded
//! - image      -> the picture, as an image part in the tool result (the SDK resolves the
//!   library id to a `data:` URL when it builds the request, so the session log keeps the id)
//! - anything else (pdf / office / video ...) -> a description plus what the model can do next
//!
//! The honest degradation is the point: claiming to have "read" a PDF it never parsed is how an
//! Agent starts inventing content.

use std::fmt::Write as _;
use std::fs;
use std::io::Read as _;

use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

use crate::artifact::{
    Artifact, ArtifactStore, LibraryEntry, human_bytes, is_inline_image, is_textual, part_for,
};

/// Default number of library entries listed.
const DEFAULT_LIMIT: usize = 20;
/// Hard cap on one listing (a library dump would eat the context).
const MAX_LIMIT: usize = 200;
/// Cap on the text handed to the model in a single read.
const MAX_TEXT_BYTES: u64 = 32 * 1024;

/// Read-only view of the media library.
pub struct MediaTool {
    store: ArtifactStore,
    definition: ToolDefinition,
}

impl MediaTool {
    #[must_use]
    pub fn new(store: ArtifactStore) -> Self {
        Self {
            store,
            definition: ToolDefinition {
                name: "media".into(),
                description: "Look at media the user sent (or that a tool produced). The files \
                    themselves are not in the conversation -- only handles are -- so this is how \
                    you actually see them. `list` shows what is available, `info` describes one \
                    entry, `read` returns the content: text for text-like files, the picture \
                    itself for images. For formats that cannot be read (pdf, office, video) it \
                    says so instead of guessing; convert or ask the user in that case."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["list", "info", "read"],
                            "description": "list = the library, newest first; info = metadata of one entry; read = the content of one entry"
                        },
                        "id": {
                            "type": "string",
                            "description": "Target entry, as returned by `list`. Required for info and read."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "list only: how many entries (default 20, max 200)"
                        }
                    },
                    "required": ["action"]
                }),
            },
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaArgs {
    action: Action,
    id: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Info,
    Read,
}

impl MediaTool {
    fn list(&self, limit: Option<usize>) -> serde_json::Value {
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let entries = self.store.list();
        if entries.is_empty() {
            return serde_json::json!({
                "content": "The media library is empty: the user has not sent any files yet."
            });
        }
        let mut text = format!(
            "{} entr{} in the media library (newest first):\n",
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        );
        for entry in entries.iter().take(limit) {
            text.push_str("- ");
            text.push_str(&entry.describe());
            text.push('\n');
        }
        if entries.len() > limit {
            let _ = writeln!(
                text,
                "({} more not shown; raise `limit` or read the one you need)",
                entries.len() - limit
            );
        }
        serde_json::json!({ "content": text })
    }

    fn info(&self, id: &str) -> Result<serde_json::Value, ToolError> {
        let entry = self.lookup(id)?;
        Ok(serde_json::json!({
            "parts": [part_for_entry(&entry)],
            "content": format!(
                "{}\nid: {}\nmime: {}\nsize: {} ({} bytes)\nadded: {}\nreadable: {}",
                entry.name,
                entry.id,
                entry.mime,
                human_bytes(entry.bytes),
                entry.bytes,
                entry.added_ms,
                if is_textual(entry.mime) {
                    "yes (text)"
                } else if is_inline_image(entry.mime) {
                    "yes (image)"
                } else {
                    "no -- convert it or ask the user"
                }
            )
        }))
    }

    fn read(&self, id: &str) -> Result<serde_json::Value, ToolError> {
        let entry = self.lookup(id)?;
        let resolved = self
            .store
            .resolve(&entry.id)
            .map_err(|error| ToolError::ExecutionError(format!("cannot read media: {error}")))?;

        if is_inline_image(entry.mime) {
            let label = format!(
                "{} ({}, {})",
                entry.name,
                entry.mime,
                human_bytes(entry.bytes)
            );
            // Two audiences, one value: `content` is what the model reads (it becomes an image
            // part in the tool message, and the SDK resolves the library id to a `data:` URL when
            // the request is assembled -- so no base64 reaches the session log), while `parts` is
            // what the UI draws (the same card the user saw when they sent it). Keeping both in
            // the envelope is what lets a tool result be *shown* instead of only described.
            return Ok(serde_json::json!({
                "content": [
                    { "type": "text", "text": label },
                    { "type": "image", "source": { "kind": "file_id", "file_id": entry.id } }
                ],
                "parts": [part_for_entry(&entry)],
            }));
        }

        if is_textual(entry.mime) {
            let file = fs::File::open(&resolved.path).map_err(|error| {
                ToolError::ExecutionError(format!("cannot open media: {error}"))
            })?;
            let mut buffer = Vec::new();
            file.take(MAX_TEXT_BYTES)
                .read_to_end(&mut buffer)
                .map_err(|error| {
                    ToolError::ExecutionError(format!("cannot read media: {error}"))
                })?;
            let text = String::from_utf8_lossy(&buffer).into_owned();
            let mut content = format!("{} ({})\n\n{text}", entry.name, entry.mime);
            if entry.bytes > MAX_TEXT_BYTES {
                let _ = write!(
                    content,
                    "\n\n[truncated: showing the first {} of {} -- read it with `read` in ranges if you need more]",
                    human_bytes(MAX_TEXT_BYTES),
                    human_bytes(entry.bytes)
                );
            }
            return Ok(serde_json::json!({
                "content": content,
                "parts": [part_for_entry(&entry)],
            }));
        }

        Ok(serde_json::json!({
            "parts": [part_for_entry(&entry)],
            "content": format!(
                "{} is {} ({}). It is stored and the user can open it, but you cannot read its \
                 contents here: only text-like files and images can be read. If you need what is \
                 inside, either ask the user, or convert it first (a converter may be available \
                 through `bash`) and then read the result.",
                entry.name,
                entry.mime,
                human_bytes(entry.bytes)
            )
        }))
    }

    fn lookup(&self, id: &str) -> Result<crate::artifact::LibraryEntry, ToolError> {
        self.store.entry(id).ok_or_else(|| {
            ToolError::ExecutionError(format!(
                "no media with id `{id}`. Call `media` with action=list to see the valid ids."
            ))
        })
    }

    fn run(&self, args: MediaArgs) -> Result<serde_json::Value, ToolError> {
        match args.action {
            Action::List => Ok(self.list(args.limit)),
            Action::Info => self.info(&required_id(args.id)?),
            Action::Read => self.read(&required_id(args.id)?),
        }
    }
}

/// The `RenderPart` for one library entry.
///
/// Reuses [`part_for`] -- the single place that decides "image card vs file card" -- so a media
/// tool result and a tool-produced artifact never render differently for the same medium.
fn part_for_entry(entry: &LibraryEntry) -> serde_json::Value {
    part_for(
        &Artifact {
            id: entry.id.clone(),
            mime: entry.mime,
            bytes: entry.bytes,
        },
        &entry.name,
        entry.bytes,
    )
}

fn required_id(id: Option<String>) -> Result<String, ToolError> {
    id.filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_owned())
        .ok_or_else(|| ToolError::ArgumentsError("`id` is required for info / read".to_owned()))
}

impl Tool for MediaTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        Box::pin(async move {
            let args: MediaArgs = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            self.run(args)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> (std::path::PathBuf, MediaTool) {
        let dir = std::env::temp_dir().join(format!(
            "shirley_media_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        let store = ArtifactStore::new(dir.clone());
        (dir, MediaTool::new(store))
    }

    async fn call(
        tool: &MediaTool,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        tool.invoke(args, ToolContext::new()).await
    }

    #[tokio::test]
    async fn an_empty_library_says_so() {
        let (_dir, tool) = fixture("empty");
        let value = call(&tool, serde_json::json!({ "action": "list" }))
            .await
            .expect("list");
        let content = value["content"].as_str().expect("content");
        assert!(content.contains("library is empty"), "got {content}");
    }

    #[tokio::test]
    async fn reading_a_text_file_returns_its_text() {
        let (dir, tool) = fixture("text");
        let entry = tool
            .store
            .put_named(
                "text/html",
                "rows.html",
                b"<table><tr><td>a</td></tr></table>",
            )
            .expect("put");
        let value = call(
            &tool,
            serde_json::json!({ "action": "read", "id": entry.id }),
        )
        .await
        .expect("read");
        let content = value["content"].as_str().expect("content");
        assert!(content.contains("rows.html"));
        assert!(content.contains("<table>"), "got {content}");
        let _ = fs::remove_dir_all(dir);
    }

    /// An image read hands the *model* an image part referencing the library -- not base64.
    #[tokio::test]
    async fn reading_an_image_returns_a_reference_not_base64() {
        let (dir, tool) = fixture("image");
        let entry = tool
            .store
            .put_named("image/png", "shot.png", b"png-bytes")
            .expect("put");
        let value = call(
            &tool,
            serde_json::json!({ "action": "read", "id": entry.id.clone() }),
        )
        .await
        .expect("read");
        // `content` is the model-facing payload: a text part plus an image part that points at
        // the library id (resolved to a `data:` URL only when the request is assembled).
        let content = value["content"].as_array().expect("array of content parts");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["kind"], "file_id");
        assert_eq!(content[1]["source"]["file_id"], entry.id.as_str());
        // `parts` is the UI-facing payload: the same card the user saw when sending it.
        let parts = value["parts"].as_array().expect("array of render parts");
        assert_eq!(parts[0]["kind"], "image");
        assert_eq!(parts[0]["source"]["kind"], "artifact");
        assert_eq!(parts[0]["source"]["id"], entry.id.as_str());
        assert!(!value.to_string().contains("base64"));
        let _ = fs::remove_dir_all(dir);
    }

    /// Honest degradation: a PDF is stored and openable, but the tool must not pretend to have
    /// read it -- that is how an Agent starts inventing content.
    #[tokio::test]
    async fn unreadable_formats_are_described_instead_of_guessed() {
        let (dir, tool) = fixture("pdf");
        let entry = tool
            .store
            .put_named("application/pdf", "report.pdf", b"%PDF-1.7")
            .expect("put");
        let value = call(
            &tool,
            serde_json::json!({ "action": "read", "id": entry.id }),
        )
        .await
        .expect("read");
        let content = value["content"].as_str().expect("content");
        assert!(content.contains("cannot read"), "got {content}");
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_unknown_id_points_at_list_instead_of_failing_obscurely() {
        let (_dir, tool) = fixture("unknown");
        let error = call(&tool, serde_json::json!({ "action": "read", "id": "nope" }))
            .await
            .expect_err("must fail");
        assert!(format!("{error}").contains("action=list"), "got {error}");
    }

    #[tokio::test]
    async fn info_reports_whether_an_entry_can_be_read() {
        let (dir, tool) = fixture("info");
        let entry = tool
            .store
            .put_named("video/mp4", "clip.mp4", b"bytes")
            .expect("put");
        let value = call(
            &tool,
            serde_json::json!({ "action": "info", "id": entry.id }),
        )
        .await
        .expect("info");
        let content = value["content"].as_str().expect("content");
        assert!(content.contains("clip.mp4"));
        assert!(content.contains("no -- convert it"), "got {content}");
        let _ = fs::remove_dir_all(dir);
    }
}
