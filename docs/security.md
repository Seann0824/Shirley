**Shirley technical design · Security and permissions (P0)**

**1. Problem statement**

The current `bash` tool is equivalent to "handing the shell to the model". The blacklist is decorative:

```rust
// src/tools/bash.rs current
let args: Vec<&str> = command.split_whitespace().collect();
let is_blacklisted = args.iter().any(|arg| BLACKLIST.contains(arg));
// ...
cmd.arg("-c").arg(&command);
```

The result of `split_whitespace` is only used for checking; what actually runs is `bash -c <original string>`. So all of the following bypass it:

- `/bin/rm -rf /` (`rm` is not a standalone token)
- `sh -c "rm -rf /"` (`rm` only appears after the quoted string is split, but the check is against `sh`, `-c`)
- `echo cm0gLXJm | base64 -d | bash`
- `r''m -rf /` (the shell concatenates it, and string matching does not see `rm`)

At the same time, the `read` tool at that time had no path restriction at all; it could read `~/.ssh/id_rsa`, and it could read `.env` to pull the API key into context and send it to the model.
(Note: `read` was once removed; it has now been reintroduced as `read` with **this section's constraints landed** -- workspace boundary validation + suspected-secret file rejection, see below.)

**2. Design goals**

1. Deny by default, allow explicitly.
2. The boundary is at the "workspace root directory" layer, not pattern matching inside the command string.
3. The permission policy is an SDK capability, not application-layer hard-coding.
4. Tool calls are auditable: who called what, whether it was blocked, and why it was blocked.

**3. Secret handling (do immediately; no code needed)**

1. Rotate `LOCAL_API_KEY` and `DEEPSEEK_API_KEY` (they once existed in cleartext in `.env`, and `Agent.md` also records the leak history).
2. Add `.env.example`, keeping only key names and placeholders:

```ini
LOCAL_API_KEY=your-key-here
LOCAL_BASE_URL=http://127.0.0.1:8788/v1/chat/completions
LOCAL_CONTEXT_WINDOW_TOKENS=104858
```

3. Add default redaction on the `read` tool side: reject when it matches patterns such as `.env`, `*.pem`, `id_rsa*`, `*.key` (**implemented**, see `src/tools/read.rs::is_secret_file`).
4. Long term: do not persist secrets to disk; inject them at startup from the environment or the system keychain.

**4. Workspace isolation**

Introduce the workspace concept on the SDK side; all file-type tools and the cwd of `bash` are constrained by it:

```rust
// crates/shirley-agent-sdk/src/workspace.rs (new)
pub struct Workspace {
    root: PathBuf,          // canonicalized absolute path
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> { /* canonicalize */ }

    // resolve and validate the path; out of bounds is Err
    pub fn resolve(&self, input: &str) -> Result<PathBuf, WorkspaceError> {
        let joined = self.root.join(input);
        let canonical = canonicalize_allow_missing(&joined)?; // handle non-existent files
        if !canonical.starts_with(&self.root) {
            return Err(WorkspaceError::OutsideRoot { requested: input.into() });
        }
        Ok(canonical)
    }
}
```

Key points:

- Use `canonicalize` to resolve `..` and symlinks before comparing; do not rely on a string prefix.
- For a write scenario where "the target file does not exist yet", `canonicalize` will fail, so canonicalize its parent directory first.
- When a path is rejected, return a structured error so the model knows it was "rejected" rather than "the file does not exist".

**5. Permission layer**

Upgrade the hard-coded blacklist into a policy object inside the SDK:

```rust
// crates/shirley-agent-sdk/src/permission.rs (new)
pub enum Decision {
    Allow,
    Deny { reason: String },
    Ask { reason: String },   // reserved: hand off to the upper layer for interactive confirmation
}

pub trait PermissionPolicy: Send + Sync {
    fn check(&self, request: &PermissionRequest) -> Decision;
}

pub struct PermissionRequest<'a> {
    pub tool: &'a str,
    pub arguments: &'a serde_json::Value,
}
```

Built-in implementation `WorkspacePolicy`:

- File-type tools: if `workspace.resolve(path)` passes, Allow; otherwise Deny.
- `bash`: do a static denial first (dangerous commands, redirection outside the workspace, `sudo`), and Allow the rest with an audit record.

**6. Rework of the `bash` tool**

**6.1 Return stderr and the exit code**

Currently only `stdout` is returned; when a build fails the model sees an empty string and gets stuck retrying the same failing command. Change it to:

```rust
#[derive(serde::Serialize)]
pub struct BashOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}
```

The model needs to see the failure information to self-correct; this is a capability issue, not just an experience issue.

**6.2 Execution method**

Keep `bash -c` (otherwise normal usage such as pipes and redirection would break), but shore up the boundary with two layers:

- **Static denial**: before `-c`, use the parsed tokens to judge dangerous patterns; for things that cannot be judged statically (such as `base64 | bash`), go to `Ask` or Deny directly.
- **Runtime constraints**: lock the cwd to the workspace; optional `ulimit`; later an OS-level sandbox can be hooked in (macOS `sandbox-exec` / Linux `bubblewrap`).

  The "OS-level sandbox" has already become its own layer with a landed skeleton; see `sandbox.md`.

**6.3 Audit**

Record `(timestamp, tool, arguments, decision, exit_code)` for each tool call to `~/.shirley/audit.log`. When a long task goes wrong, this is the only basis for retrospective analysis.

**7. Acceptance**

1. `/bin/rm -rf /`, `echo ... | base64 -d | bash` are Denied with a reason.
2. `read ~/.ssh/id_rsa`, `read ../../etc/passwd` are Denied.
3. When `cargo build` fails, the model receives stderr and the exit code.
4. The audit log contains all of the above blocked records.
