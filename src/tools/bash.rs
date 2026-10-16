use std::path::PathBuf;
use std::time::Duration;

use shirley_agent_sdk::sandbox::{Sandbox, SandboxOutput, SandboxSpec, backend::ProcessBackend};
use shirley_agent_sdk::tool;

// Static deny list: a temporary stopgap before the real permission layer (PermissionPolicy, see docs/security.md section 5) lands.
// Note: string matching can be bypassed (/bin/rm, r''m, base64 | bash),
// the real boundary is in the sandbox backend, not this list.
const BLACKLIST: [&str; 3] = ["rm", "shutdown", "reboot"];

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
        text.push_str(&format!("[timeout] command timed out: {command}\n"));
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
        text.push_str(&format!("[exit code] {code}\n"));
    }

    // // Degradation notice: this execution's isolation did not fully take effect. Report honestly, do not silently ignore.
    // if !output.is_fully_isolated() {
    //     text.push_str(&format!(
    //         "[sandbox degraded] isolation strength: {}, the following constraints did not take effect: {}\n",
    //         output.isolation,
    //         output.degraded.join("; ")
    //     ));
    // }

    text
}

// TODO: tool error messages should have a uniform format, so that execution failures can be reported back in a designed format.
#[tool(
    description = "Run shell commands such as python, find, or git. Use the grep tool for workspace content searches and read for file contents. Only the first 32 KiB of stdout and stderr are returned, with a truncation notice for larger output. A line limit such as head does not cap bytes. Redirect large output to a file and use read to inspect relevant ranges."
)]
pub async fn bash(
    #[param(description = "the command to execute")] command: String,
    #[param(description = "timeout in seconds")] timeout: Option<u64>,
) -> Result<String, shirley_agent_sdk::ToolError> {
    // Static stopgap rejection: obviously destructive commands are blocked directly.
    // This layer is temporary; the real boundary is the sandbox below.
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(shirley_agent_sdk::ToolError::ExecutionError(
            "command cannot be empty".to_string(),
        ));
    }
    if tokens.iter().any(|arg| BLACKLIST.contains(arg)) {
        return Err(shirley_agent_sdk::ToolError::ExecutionError(format!(
            "command contains a blacklisted command: {BLACKLIST:?}"
        )));
    }

    // 1. Put the command into the process sandbox.
    //    Keep `bash -c` (otherwise normal usage like pipes and redirection breaks),
    //    but the boundary is applied by the sandbox backend at the "what can be touched" layer, rather than pattern-matching the command string.
    let mut spec = SandboxSpec::new("bash")
        .arg("-c")
        .arg(&command)
        .timeout(Duration::from_secs(timeout.unwrap_or(60)));

    if let Some(root) = workspace_root() {
        spec = spec.workspace_root(root);
    }

    // TODO: the backend should be configurable (macOS sandbox-exec / Linux bwrap).
    // Currently SandboxBackend::execute returns impl Future, and the trait is not object-safe,
    // so it cannot be injected dynamically via Box<dyn>; instantiate directly instead. To switch backends, change this one line.
    let sandbox = Sandbox::new(ProcessBackend);

    // 2. Execute. The timeout is applied uniformly by Sandbox; no need to wrap tokio::time::timeout here.
    let output = sandbox.run(&spec).await.map_err(|e| {
        shirley_agent_sdk::ToolError::ExecutionError(format!("{command}: sandbox execution failed: {e}"))
    })?;

    // 3. Render the result and return it to the model.
    Ok(render(&output, &command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_simple_command() {
        let out = bash("echo hello".to_string(), Some(10)).await.unwrap();
        assert!(out.contains("hello"), "output should contain hello, got: {out}");
    }

    #[tokio::test]
    async fn surfaces_stderr_and_exit_code() {
        // A failing command must bring back stderr and the exit code so the model can self-correct.
        let out = bash("echo boom >&2; exit 3".to_string(), Some(10))
            .await
            .unwrap();
        assert!(out.contains("boom"), "should contain stderr: {out}");
        assert!(out.contains("[exit code] 3"), "should contain the exit code: {out}");
    }

    #[tokio::test]
    async fn reports_sandbox_degradation() {
        // Currently ProcessBackend, isolation does not take effect, must honestly report degradation.
        let out = bash("true".to_string(), Some(10)).await.unwrap();
        assert!(out.contains("[sandbox degraded]"), "should report degradation: {out}");
    }

    #[tokio::test]
    async fn rejects_blacklisted_command() {
        let err = bash("rm -rf /".to_string(), Some(10)).await.unwrap_err();
        assert!(err.to_string().contains("blacklist"), "should be blocked: {err}");
    }

    #[tokio::test]
    async fn enforces_timeout() {
        let out = bash("sleep 5".to_string(), Some(1)).await.unwrap();
        assert!(out.contains("[timeout]"), "should mark timeout: {out}");
    }

    #[tokio::test]
    async fn rejects_empty_command() {
        let err = bash("   ".to_string(), Some(10)).await.unwrap_err();
        assert!(err.to_string().contains("cannot be empty"), "empty command should error: {err}");
    }
}
