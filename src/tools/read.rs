use std::path::{Path, PathBuf};

use shirley_agent_sdk::workspace::{WorkSpace, WorkspaceError};
use shirley_agent_sdk::{ToolContext, ToolError, tool};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};

/// Default line count. Used when the model does not specify, to avoid "reading the whole file by the way".
const DEFAULT_LINE_COUNT: usize = 200;

/// Hard cap on lines per read. Even if the model explicitly asks for more, it will not flood the context in one go.
const MAX_LINE_COUNT: usize = 2000;

/// Byte cap per output. When the line count is within limits but the content is too long (e.g. a very long compressed file, a huge single line),
/// this layer truncates as a fallback, keeping context usage bounded.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Binary sniff window: if a NUL appears in this many leading bytes, treat it as a binary file and reject.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Workspace root: reads the `SHIRLEY_WORKSPACE` env var first, otherwise falls back to the current directory.
/// Kept consistent with the `bash` tool to avoid drift between the two.
pub(crate) fn workspace_root() -> Option<PathBuf> {
    std::env::var("SHIRLEY_WORKSPACE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// The Agent's real workspace root, injected into the [`ToolContext`] by the application layer
/// (`bootstrap::build_agent_with`).
///
/// `read` / `grep` used to derive their root from the `SHIRLEY_WORKSPACE` env var (falling back to
/// the process CWD), which is decoupled from the `Agent`'s `working_dir`. Whenever the two differ
/// (e.g. the desktop interface's multi-project mode: one process, several project roots), a relative
/// path was resolved against the wrong directory and reported as an error. Carrying the real root
/// here keeps the tools aligned with the Agent.
#[derive(Clone, Debug)]
pub struct WorkspaceRoot(pub PathBuf);

/// Resolve the workspace root the tools should operate against, preferring the [`ToolContext`]
/// value the application layer injected, then falling back to the legacy `SHIRLEY_WORKSPACE` env
/// var / process CWD (so the tools still work when invoked without a context, e.g. unit tests).
pub(crate) fn resolve_workspace_root(ctx: &ToolContext) -> Option<PathBuf> {
    ctx.get::<WorkspaceRoot>()
        .map(|root| root.0.clone())
        .or_else(workspace_root)
}

/// Suspected secret file: rejected by default, to avoid reading `.env` / private keys into the context and then sending them to the model
/// (see `docs/security.md` section 3).
pub(crate) fn is_secret_file(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name == ".netrc"
        || name == "credentials"
        || name.starts_with("id_rsa")
        || name.starts_with("id_dsa")
        || name.starts_with("id_ecdsa")
        || name.starts_with("id_ed25519")
}

/// Clip `text` at a character boundary to no more than `budget` bytes.
pub(crate) fn clip_to_budget(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        if index + ch.len_utf8() > budget {
            break;
        }
        end = index + ch.len_utf8();
    }
    &text[..end]
}

/// Read a text file inside the workspace by line, returning a line-numbered excerpt.
///
/// Its reason to exist is to **make reading controllable**: paths are constrained to the workspace, and both line count and byte count have
/// hard caps, so the model no longer needs to (and should not) read a whole file raw with `cat` and flood the context.
///
/// Returning plain text rather than structured JSON is deliberate -- directly readable by the model, and the line numbers make it easy to
/// continue reading with `start_line`.
#[tool(
    description = "Read the contents of a text file inside the workspace by line, returning line-numbered text. Reads have line and byte caps; when content is truncated, a hint on how to continue is shown at the end. For large files, first locate content with the grep tool, then use this tool to read the relevant range; do not read the whole file with cat."
)]
pub async fn read(
    ctx: &ToolContext,
    #[param(description = "file path, relative to the workspace root; absolute paths or paths escaping the workspace are not accepted")]
    path: String,
    #[param(description = "start line, 1-based; defaults to line 1")]
    start_line: Option<usize>,
    #[param(description = "maximum number of lines to read, defaults to 200, hard cap 2000")]
    line_count: Option<usize>,
) -> Result<String, ToolError> {
    let root = resolve_workspace_root(ctx)
        .ok_or_else(|| ToolError::ExecutionError("cannot determine the workspace root".into()))?;
    read_text_range(&root, &path, &path, start_line, line_count).await
}

/// Controlled read kernel: reads the relative path `path` under `root` by line, returning a line-numbered excerpt.
///
/// This is the **only read implementation** for `read` (root = workspace) and `read_skill_file` (root = skill directory)
/// -- the boundary lives in one place (`docs/security.md`): paths constrained to `root`, hard caps on line /
/// byte counts, suspected-secret rejection, binary sniffing. `display` is the path name shown to the model
/// (a relative reference), kept separate from `path`, so skill scenarios can show forms like `scripts/foo.py`.
pub(crate) async fn read_text_range(
    root: &Path,
    path: &str,
    display: &str,
    start_line: Option<usize>,
    line_count: Option<usize>,
) -> Result<String, ToolError> {
    let start_line = start_line.unwrap_or(1);
    if start_line == 0 {
        return Err(ToolError::ArgumentsError("start line must be greater than 0".into()));
    }
    let requested = line_count.unwrap_or(DEFAULT_LINE_COUNT);
    if requested == 0 {
        return Err(ToolError::ArgumentsError("line count must be greater than 0".into()));
    }
    let count = requested.min(MAX_LINE_COUNT);

    // 1. Path first goes through the root constraint: absolute paths and `..` escapes are both rejected here.
    let workspace = WorkSpace::new(root.to_path_buf())
        .map_err(|error| ToolError::ExecutionError(format!("workspace initialization failed: {error}")))?;
    let resolved = workspace.resolve(path).map_err(|error| match error {
        // Escape / invalid path are both request-side problems: the model can succeed by changing the path or its spelling,
        // so classify as an argument error rather than an execution failure.
        WorkspaceError::OutsideRoot(_) | WorkspaceError::InvalidPath(_) => {
            ToolError::ArgumentsError(error.to_string())
        }
        WorkspaceError::Io(_) => ToolError::ExecutionError(error.to_string()),
    })?;

    // 2. Secret files are rejected by default.
    if let Some(name) = resolved.file_name().and_then(|name| name.to_str())
        && is_secret_file(name)
    {
        return Err(ToolError::ArgumentsError(format!(
            "refusing to read a suspected secret file: {display}"
        )));
    }

    // 3. Directories / special files get clear guidance, rather than a baffling read failure.
    let metadata = tokio::fs::metadata(&resolved)
        .await
        .map_err(|error| ToolError::ExecutionError(format!("failed to read metadata of {display}: {error}")))?;
    if metadata.is_dir() {
        return Err(ToolError::ExecutionError(format!(
            "{display} is a directory; use bash's ls to list a directory, this tool reads files only"
        )));
    }
    if !metadata.is_file() {
        return Err(ToolError::ExecutionError(format!(
            "{display} is not a regular file and cannot be read"
        )));
    }

    // 4. Open and do binary sniffing.
    let mut file = tokio::fs::File::open(&resolved)
        .await
        .map_err(|error| ToolError::ExecutionError(format!("failed to open file {display}: {error}")))?;
    let mut sniff = vec![0u8; BINARY_SNIFF_BYTES];
    let sniffed = file
        .read(&mut sniff)
        .await
        .map_err(|error| ToolError::ExecutionError(format!("failed to read file {display}: {error}")))?;
    if sniff[..sniffed].contains(&0) {
        return Err(ToolError::ExecutionError(format!(
            "{display} looks like a binary file; read refused"
        )));
    }
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|error| ToolError::ExecutionError(format!("failed to seek in file {display}: {error}")))?;

    // 5. Skip to the start line.
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let mut line_no = 1usize;
    while line_no < start_line {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .await
            .map_err(|error| ToolError::ExecutionError(format!("failed to read file {display}: {error}")))?;
        if read == 0 {
            return Ok(format!(
                "[{display} ended before line {start_line}; nothing to read]"
            ));
        }
        line_no += 1;
    }

    // 6. Read line by line, bounded by both the line count and byte caps.
    let mut out = String::new();
    let mut emitted = 0usize;
    let mut truncated = false;
    let mut more_lines = false;
    loop {
        if emitted >= count {
            // Line count reached; probe whether there is more content to decide whether to hint continuation.
            buffer.clear();
            let read = reader.read_until(b'\n', &mut buffer).await.map_err(|error| {
                ToolError::ExecutionError(format!("failed to read file {display}: {error}"))
            })?;
            more_lines = read > 0;
            break;
        }

        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .await
            .map_err(|error| ToolError::ExecutionError(format!("failed to read file {display}: {error}")))?;
        if read == 0 {
            break;
        }

        let text = String::from_utf8_lossy(&buffer);
        let text = text.trim_end_matches(['\n', '\r']);
        let prefix = format!("{line_no:>6} | ");

        let remaining = MAX_OUTPUT_BYTES.saturating_sub(out.len());
        // Leave at least room for the prefix and newline; otherwise this line does not fit, truncate directly.
        if remaining <= prefix.len() + 1 {
            truncated = true;
            break;
        }
        out.push_str(&prefix);

        let budget = remaining - prefix.len() - 1;
        let clipped = clip_to_budget(text, budget);
        out.push_str(clipped);
        out.push('\n');

        emitted += 1;
        if clipped.len() < text.len() {
            // A single line exceeds the remaining byte budget; this line was truncated.
            truncated = true;
            break;
        }
        line_no += 1;
    }

    if more_lines {
        truncated = true;
    }

    if emitted == 0 {
        return Ok(format!(
            "[{display} ended before line {start_line}; nothing to read]"
        ));
    }

    let last_line = start_line + emitted - 1;
    let mut result = format!("# {display} (lines {start_line}-{last_line})\n{out}");
    if truncated {
        let next = last_line + 1;
        result.push_str(&format!(
            "[content truncated; to continue, use start_line={next}]\n"
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests mutate the process-level workspace env var; run serially to avoid interference.
    // Use tokio's Mutex: the guard may be held across await, std's would be flagged by clippy.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Create a unique temporary workspace directory.
    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_read_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn reads_lines_with_line_numbers() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("basic");
        std::fs::write(dir.join("a.txt"), "first\nsecond\nthird\n").unwrap();
        // SAFETY: env var is changed serially within the test, not concurrently with other threads.
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let out = read(&ToolContext::new(), "a.txt".into(), None, None).await.unwrap();

        assert!(out.contains("     1 | first"), "should include line numbers: {out}");
        assert!(out.contains("     3 | third"), "should read the third line: {out}");
        assert!(!out.contains("truncated"), "should not hint truncation when content is complete: {out}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn supports_start_line_and_continuation() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("range");
        let body: String = (1..=10).map(|n| format!("line{n}\n")).collect();
        std::fs::write(dir.join("b.txt"), body).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let out = read(&ToolContext::new(), "b.txt".into(), Some(3), Some(2)).await.unwrap();

        assert!(out.contains("     3 | line3"), "should start at line 3: {out}");
        assert!(out.contains("     4 | line4"), "should read two lines: {out}");
        assert!(!out.contains("line5"), "should not exceed the line cap: {out}");
        assert!(out.contains("start_line=5"), "should hint the continuation position: {out}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_outside_workspace() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("escape");
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read(&ToolContext::new(), "../etc/passwd".into(), None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::ArgumentsError(_)),
            "escape should be an argument error: {err:?}"
        );
        assert!(err.to_string().contains("outside workspace"), "should explain the escape: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_absolute_path() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("absolute");
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read(&ToolContext::new(), "/etc/hosts".into(), None, None).await.unwrap_err();
        assert!(
            matches!(err, ToolError::ArgumentsError(_)),
            "absolute path should be rejected: {err:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_secret_file() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("secret");
        std::fs::write(dir.join(".env"), "LOCAL_API_KEY=leak").unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read(&ToolContext::new(), ".env".into(), None, None).await.unwrap_err();
        assert!(err.to_string().contains("secret"), "should reject secret files: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_binary_file() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("binary");
        std::fs::write(dir.join("bin"), [0u8, 1, 2, 3, 0, 255]).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read(&ToolContext::new(), "bin".into(), None, None).await.unwrap_err();
        assert!(err.to_string().contains("binary"), "should reject binary: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_directory() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("dir");
        std::fs::create_dir(dir.join("sub")).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read(&ToolContext::new(), "sub".into(), None, None).await.unwrap_err();
        assert!(err.to_string().contains("directory"), "should guide to use ls: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn caps_output_bytes() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("bytes");
        // A single line already exceeds the byte cap; it must be truncated rather than stuffed into the context whole.
        let big = "a".repeat(MAX_OUTPUT_BYTES * 2);
        std::fs::write(dir.join("big.txt"), big).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let out = read(&ToolContext::new(), "big.txt".into(), None, None).await.unwrap();
        assert!(out.contains("truncated"), "should hint truncation: length {}", out.len());
        assert!(
            out.len() <= MAX_OUTPUT_BYTES + 200,
            "output should be bounded by the byte cap, actual {}",
            out.len()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn registers_and_invokes_through_tool_manager() {
        use shirley_agent_sdk::{ToolCall, ToolManager};

        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("manager");
        std::fs::write(dir.join("m.txt"), "alpha\nbeta\n").unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        // Use the macro-generated `tool()` to verify the full path of registration + argument deserialization + invocation.
        let mut manager = ToolManager::new();
        manager.register(read::tool()).expect("registration should succeed");

        let definition = manager
            .definitions()
            .into_iter()
            .find(|d| d.name == "read")
            .expect("should get the read definition");
        assert!(manager.definitions().iter().all(|tool| tool.name != "read_file"));
        assert!(
            definition.description.contains("line"),
            "the description should explain the read semantics: {}",
            definition.description
        );

        let call = ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: r#"{"path":"m.txt","start_line":1,"line_count":1}"#.into(),
        };
        let output = manager
            .invoke(&call, shirley_agent_sdk::ToolContext::new())
            .await
            .expect("invocation should succeed");
        let text = output.as_str().unwrap_or_default();
        assert!(text.contains("alpha"), "should read the content: {text}");
        assert!(!text.contains("beta"), "should be bounded by the line count: {text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn caps_line_count() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("lines");
        let body: String = (1..=MAX_LINE_COUNT + 50)
            .map(|n| format!("l{n}\n"))
            .collect();
        std::fs::write(dir.join("many.txt"), body).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        // Explicitly request far more lines than the cap; still only read up to the cap.
        let out = read(&ToolContext::new(), "many.txt".into(), None, Some(MAX_LINE_COUNT * 2))
            .await
            .unwrap();
        let lines = out.lines().filter(|l| l.contains(" | ")).count();
        assert_eq!(lines, MAX_LINE_COUNT, "should read exactly the line cap");
        assert!(out.contains("truncated"), "should hint there is more: {out}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
