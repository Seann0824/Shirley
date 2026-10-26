use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use shirley_agent_sdk::sandbox::{Sandbox, SandboxOutput, SandboxSpec, backend::ProcessBackend};
use shirley_agent_sdk::{ToolContext, tool};

/// Workspace root: reads the `SHIRLEY_WORKSPACE` env var first, otherwise falls back to the current directory.
///
/// The sandbox uses it as the cwd and (in a real backend) restricts filesystem visibility based on it.
fn workspace_root() -> Option<PathBuf> {
    std::env::var("SHIRLEY_WORKSPACE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// Render the sandbox result into text the model can read.
///
/// The model needs to see stderr and the exit code to self-correct (docs/security.md 6.1);
/// degradation info honestly reports "isolation did not take effect this time", so the upper layer does not mistakenly think the command was constrained.
fn render(output: &SandboxOutput, command: &str) -> String {
    let mut text = String::new();

    if output.timed_out {
        let _ = writeln!(text, "[timeout] command timed out: {command}");
    }

    if !output.stdout.is_empty() {
        text.push_str(&output.stdout);
        if !output.stdout.ends_with('\n') {
            text.push('\n');
        }
    }

    if !output.stderr.is_empty() {
        text.push_str("[stderr]\n");
        text.push_str(&output.stderr);
        if !output.stderr.ends_with('\n') {
            text.push('\n');
        }
    }

    if let Some(code) = output.exit_code
        && code != 0
    {
        let _ = writeln!(text, "[exit code] {code}");
    }

    // Degradation notice: this execution's isolation did not fully take effect.
    // Report honestly, do not silently ignore.
    if !output.is_fully_isolated() {
        let _ = writeln!(
            text,
            "[sandbox degraded] isolation strength: {}, the following constraints did not take effect: {}",
            output.isolation,
            output.degraded.join("; ")
        );
    }

    text
}

/// A command is often a skill script that writes something the human must *look at*: a login QR
/// code awaiting a scan, a generated PDF or deck, a rendered clip. Making the model spend an
/// extra round-trip on `read` just to surface it is waste, and the model may not think of it at
/// all, so after the command finishes we look for products written during this run and attach
/// them to the same tool result as render parts (`docs/render-parts.md`).
///
/// "Product" is decided by mime (`artifact::is_attachable`), not by extension list here: images,
/// PDF, office documents, media, archives. Text (`txt`/`md`/`json`/`csv`) is deliberately
/// excluded -- build steps litter those and the model reads them on demand.
///
/// Scope is deliberately boring: the workspace root and its immediate children, newest first, at
/// most three. Deeper files stay reachable through `read`; this is a convenience for the common
/// "script dropped a file next to me" case, not a crawler.
const MAX_ATTACHED_PRODUCTS: usize = 3;
const MAX_SCANNED_ENTRIES: usize = 2000;
/// File timestamps are coarse and the sandbox hands off between processes; be generous rather
/// than miss a file written a moment before we started.
const MTIME_SLACK: Duration = Duration::from_secs(2);
const SKIPPED_DIRS: [&str; 5] = ["node_modules", "target", ".git", "dist", "build"];

/// Products written under `root` (its immediate children included) at or after `since`.
fn collect_new_products(root: &Path, since: SystemTime) -> Vec<(PathBuf, &'static str)> {
    let cutoff = since.checked_sub(MTIME_SLACK).unwrap_or(since);

    let mut dirs = vec![root.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_ref()) {
                continue;
            }
            if entry.metadata().is_ok_and(|meta| meta.is_dir()) {
                dirs.push(entry.path());
            }
        }
    }

    let mut found: Vec<(SystemTime, PathBuf, &'static str)> = Vec::new();
    let mut budget = MAX_SCANNED_ENTRIES;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                break;
            }
            budget -= 1;

            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let Some(mime) = crate::artifact::mime_for_path(&path) else {
                continue;
            };
            if !crate::artifact::is_attachable(mime) {
                continue;
            }
            let Ok(modified) = meta.modified() else {
                continue;
            };
            if modified < cutoff {
                continue;
            }
            found.push((modified, path, mime));
        }
    }

    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.truncate(MAX_ATTACHED_PRODUCTS);
    found
        .into_iter()
        .map(|(_, path, mime)| (path, mime))
        .collect()
}

/// Attach freshly written products to the command result.
///
/// Returns a plain string when there is nothing to show, so the model's view of an ordinary
/// command is byte-for-byte what it was before this feature existed (prefix cache stays intact).
fn attach_products(
    ctx: &ToolContext,
    root: &Path,
    since: SystemTime,
    mut text: String,
) -> serde_json::Value {
    let Some(store) = ctx.get::<crate::artifact::ArtifactStore>() else {
        return serde_json::Value::String(text);
    };

    let mut parts = Vec::new();
    for (path, mime) in collect_new_products(root, since) {
        let Ok(artifact) = store.put_file(&path) else {
            continue;
        };
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().to_string(),
        );
        let _ = writeln!(
            text,
            "[file] {name} ({mime}, {} KB) - already shown to the human in the chat. Do not read \
             the bytes back; refer to it or let the human open it.",
            artifact.bytes / 1024
        );
        parts.push(crate::artifact::part_for(&artifact, &name, artifact.bytes));
    }

    if parts.is_empty() {
        serde_json::Value::String(text)
    } else {
        serde_json::json!({ "content": text, "parts": parts })
    }
}

// TODO: tool error messages should have a uniform format, so that execution failures can be reported back in a designed format.
#[tool(
    description = "Run shell commands such as python, find, or git. Use the grep tool for workspace content searches and read for file contents. Only the first 32 KiB of stdout and stderr are returned, with a truncation notice for larger output. A line limit such as head does not cap bytes. Redirect large output to a file and use read to inspect relevant ranges. Files the command produces (images, PDFs, decks, clips) are attached to the chat for the human automatically."
)]
pub async fn bash(
    ctx: &ToolContext,
    #[param(description = "the command to execute")] command: String,
    #[param(description = "timeout in seconds")] timeout: Option<u64>,
) -> Result<serde_json::Value, shirley_agent_sdk::ToolError> {
    if command.trim().is_empty() {
        return Err(shirley_agent_sdk::ToolError::ExecutionError(
            "command cannot be empty".to_string(),
        ));
    }

    // 1. Put the command into the process sandbox.
    //    Keep `bash -c` (otherwise normal usage like pipes and redirection breaks),
    //    ProcessBackend currently provides execution and timeouts without filesystem/network isolation.
    let mut spec = SandboxSpec::new("bash")
        .arg("-c")
        .arg(&command)
        .timeout(Duration::from_secs(timeout.unwrap_or(60)));

    let root = workspace_root();
    if let Some(root) = root.as_ref() {
        spec = spec.workspace_root(root.clone());
    }

    // Everything created at or after this instant is a product of this very command.
    let started = SystemTime::now();

    // TODO: the backend should be configurable (macOS sandbox-exec / Linux bwrap).
    // Currently SandboxBackend::execute returns impl Future, and the trait is not object-safe,
    // so it cannot be injected dynamically via Box<dyn>; instantiate directly instead. To switch backends, change this one line.
    let sandbox = Sandbox::new(ProcessBackend);

    // 2. Execute. The timeout is applied uniformly by Sandbox; no need to wrap tokio::time::timeout here.
    let output = sandbox.run(&spec).await.map_err(|e| {
        shirley_agent_sdk::ToolError::ExecutionError(format!(
            "{command}: sandbox execution failed: {e}"
        ))
    })?;

    // 3. Render the result for the model, and attach anything visual the command just produced
    //    so the human can look at it without an extra tool round-trip.
    let text = render(&output, &command);
    Ok(match root.as_ref() {
        Some(root) => attach_products(ctx, root, started, text),
        None => serde_json::Value::String(text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model-visible text of a result: a bare string, or the `content` field of the
    /// `{ content, parts }` envelope a command that produced images returns.
    fn text(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(text) => text.clone(),
            other => other["content"].as_str().unwrap_or_default().to_string(),
        }
    }

    #[tokio::test]
    async fn runs_simple_command() {
        let out = bash(&ToolContext::new(), "echo hello".to_string(), Some(10))
            .await
            .unwrap();
        assert!(
            text(&out).contains("hello"),
            "output should contain hello, got: {out}"
        );
    }

    #[tokio::test]
    async fn surfaces_stderr_and_exit_code() {
        // A failing command must bring back stderr and the exit code so the model can self-correct.
        let out = bash(
            &ToolContext::new(),
            "echo boom >&2; exit 3".to_string(),
            Some(10),
        )
        .await
        .unwrap();
        assert!(text(&out).contains("boom"), "should contain stderr: {out}");
        assert!(
            text(&out).contains("[exit code] 3"),
            "should contain the exit code: {out}"
        );
    }

    #[tokio::test]
    async fn reports_sandbox_degradation() {
        // Currently ProcessBackend, isolation does not take effect, must honestly report degradation.
        let out = bash(&ToolContext::new(), "true".to_string(), Some(10))
            .await
            .unwrap();
        assert!(
            text(&out).contains("[sandbox degraded]"),
            "should report degradation: {out}"
        );
    }

    #[tokio::test]
    async fn enforces_timeout() {
        let out = bash(&ToolContext::new(), "sleep 5".to_string(), Some(1))
            .await
            .unwrap();
        assert!(
            text(&out).contains("[timeout]"),
            "should mark timeout: {out}"
        );
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_bash_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn attaches_images_the_command_just_produced() {
        // The login-QR case: a script drops an image, the human must be able to look at it in
        // the chat without the model spending a round-trip on `read`.
        let root = temp_dir("attach");
        let store = crate::artifact::ArtifactStore::new(root.join("artifacts"));
        let ctx = ToolContext::new().with(store);

        std::fs::write(root.join("qr.png"), b"\x89PNG\r\n\x1a\n").expect("write");

        let since = SystemTime::now() - Duration::from_secs(30);
        let value = attach_products(&ctx, &root, since, "saved qr.png".to_string());

        assert_eq!(value["parts"][0]["kind"], "image", "got: {value}");
        assert_eq!(value["parts"][0]["mime"], "image/png");
        assert!(
            value["parts"][0]["source"]["id"].is_string(),
            "must reference the artifact, not the bytes: {value}"
        );
        // The model reads the text, not the parts: the envelope carries both.
        assert!(
            value["content"]
                .as_str()
                .is_some_and(|text| text.contains("[file] qr.png")),
            "got: {value}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn leaves_non_image_products_alone() {
        let root = temp_dir("noattach");
        let store = crate::artifact::ArtifactStore::new(root.join("artifacts"));
        let ctx = ToolContext::new().with(store);

        std::fs::write(root.join("report.txt"), b"hello").expect("write");

        let since = SystemTime::now() - Duration::from_secs(30);
        let value = attach_products(&ctx, &root, since, "done".to_string());

        // Nothing visual was produced: the result stays a bare string, so the model's view of
        // an ordinary command is byte-for-byte unchanged.
        assert_eq!(value, serde_json::Value::String("done".to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn rejects_empty_command() {
        let err = bash(&ToolContext::new(), "   ".to_string(), Some(10))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("cannot be empty"),
            "empty command should error: {err}"
        );
    }
}
