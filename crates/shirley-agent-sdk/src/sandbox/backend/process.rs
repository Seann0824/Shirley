//! The most bare-bones backend: spawn a process directly, **with no isolation at all**.
//!
//! Its existence serves two purposes:
//! 1. Get the whole pipeline (spec -> execute -> output) working and testable first.
//! 2. Serve as the "degraded baseline" — it faithfully writes every constraint it cannot satisfy
//!    into `degraded`, so the caller sees degraded and knows "this run is a bare process, don't take
//!    it seriously".
//!
//! Never use it to execute untrusted input.

// Stdio: controls how the subprocess's stdin/stdout/stderr are wired.
use std::fmt::Write as _;
use std::process::Stdio;

// tokio's async Command: used to spawn the subprocess (async = does not block while waiting for it
// to finish).
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

// Bring in the interface to implement and the types to use.
use crate::sandbox::backend::{Capabilities, SandboxBackend, SandboxError};
use crate::sandbox::output::SandboxOutput;
use crate::sandbox::spec::{NetworkPolicy, SandboxSpec};

// Define an empty struct (no fields) — it is just a "marker" and needs to store no data.
// derive(Default) lets ProcessBackend::default() create it.
#[derive(Default, Debug)]
pub struct ProcessBackend;

/// Retain at most 32 KiB from each pipe. A line limit cannot bound minified JS or
/// source maps: a single line can be many megabytes long.
const MAX_CAPTURE_BYTES: usize = 32 * 1024;

async fn capture_output(mut pipe: impl AsyncRead + Unpin) -> std::io::Result<String> {
    let mut captured = Vec::with_capacity(MAX_CAPTURE_BYTES);
    let mut omitted = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let len = pipe.read(&mut buffer).await?;
        if len == 0 {
            break;
        }
        let keep = len.min(MAX_CAPTURE_BYTES - captured.len());
        captured.extend_from_slice(&buffer[..keep]);
        omitted = omitted.saturating_add((len - keep) as u64);
        // Keep draining after the cap so a full pipe cannot block the command.
    }
    let mut text = String::from_utf8_lossy(&captured).into_owned();
    if omitted > 0 {
        let _ = writeln!(
            text,
            "\n[output truncated: {omitted} bytes omitted; capture limit is {MAX_CAPTURE_BYTES} bytes per stream. Narrow the query or redirect output to a file and read a relevant range.]"
        );
    }
    Ok(text)
}

// `impl SandboxBackend for ProcessBackend` = "make ProcessBackend satisfy the SandboxBackend
// contract". The braces must implement every method the trait requires.
impl SandboxBackend for ProcessBackend {
    // Implement capabilities: report that it "can isolate nothing".
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            filesystem_isolation: false,  // cannot isolate the filesystem
            network_isolation: false,     // cannot isolate the network
            resource_limits: false,       // cannot limit resources
            label: "none (bare process)", // label: bare process
        }
    }

    // Implement name: give a short name.
    fn name(&self) -> &'static str {
        "process"
    }

    // Implement execute: the place that actually does the work.
    // async means this is an async function; &self borrows self; spec borrows the spec to read it.
    async fn execute(&self, spec: &SandboxSpec) -> Result<SandboxOutput, SandboxError> {
        // Defensive check: if no program name is given, error out immediately.
        // is_empty() checks whether the string is empty.
        if spec.program.is_empty() {
            // return Err(...) means "fail early and exit".
            // "program is empty".into() turns &str into String.
            return Err(SandboxError::Spawn("program is empty".into()));
        }

        // Create a command to execute spec.program.
        // &spec.program is a borrow (does not take program out of spec).
        let mut cmd = Command::new(&spec.program);

        // Pass each argument in spec.args to the command one by one.
        // &spec.args borrows the whole argument list.
        cmd.args(&spec.args);

        // kill_on_drop(true): if this cmd object is dropped, kill the subprocess.
        // This is the key to the timeout fallback — an outer timeout drops the future, and the
        // subprocess is killed along with it.
        cmd.kill_on_drop(true);

        // Environment variables are not inherited by default: clear them first.
        // Avoid leaking the host's sensitive variables like API keys to the subprocess.
        cmd.env_clear();

        // Then inject the environment variables explicitly required by the spec, one by one.
        // `for (k, v) in &spec.env` is "iterate over each key-value pair in the list".
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        // If the caller did not set PATH itself, provide a minimal PATH.
        // Otherwise the program cannot find its own dependencies (e.g. python3).
        // .iter() iterates; .any(...) checks "whether any item satisfies the condition".
        if !spec.env.iter().any(|(k, _)| k == "PATH") {
            // Read the host's current PATH; fall back to an empty string if it cannot be read.
            cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
        }

        // Set the working directory: prefer spec.cwd, else spec.workspace_root.
        // .as_ref() turns Option<PathBuf> into Option<&PathBuf> (a borrow, not taking ownership).
        // .or(...) means "use the former if it has a value, otherwise the latter".
        if let Some(cwd) = spec.cwd.as_ref().or(spec.workspace_root.as_ref()) {
            cmd.current_dir(cwd); // set the subprocess's working directory
        }

        // Wire stdin to null (the subprocess reads no input).
        cmd.stdin(Stdio::null());
        // Wire stdout/stderr to pipes (so we can read their output in the parent process).
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Drain both pipes concurrently with bounded buffers. Command::output()
        // buffers everything before returning, which can flood memory and context.
        let mut child = cmd
            .spawn()
            .map_err(|e| SandboxError::Spawn(format!("failed to spawn {}: {e}", spec.program)))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let (status, stdout, stderr) =
            tokio::try_join!(child.wait(), capture_output(stdout), capture_output(stderr),)
                .map_err(|e| {
                    SandboxError::Spawn(format!("failed to capture {}: {e}", spec.program))
                })?;

        // The process has finished by now. Next, "faithfully report the degradation entries".

        // First build a list with two "never achievable" items.
        // vec![...] is the literal form for creating a list.
        // .to_owned() turns &str into String.
        let mut degraded = vec![
            "no filesystem isolation: the process can access the host filesystem".to_owned(),
            "no resource limits: no memory/process-count caps applied".to_owned(),
        ];

        // Add a corresponding degradation note based on the network policy the spec requires.
        match spec.network {
            // If the caller asked for no network but we cannot do it, say so faithfully.
            NetworkPolicy::Disabled => degraded
                .push("no network isolation: NetworkPolicy::Disabled is not enforced".to_owned()),
            // If a proxy is required, we likewise cannot do it; say so faithfully.
            // `{ .. }` means "we don't care about the addr value inside; ignore it".
            NetworkPolicy::Proxy { .. } => {
                degraded.push("no network isolation: proxy egress is not enforced".to_owned());
            }
        }

        // If the caller set a workspace root, it thinks there is a filesystem constraint —
        // but we only set cwd there; we did not actually lock down the filesystem, so we must say
        // so.
        if spec.workspace_root.is_some() {
            degraded.push("workspace is only a cwd; not enforced by the filesystem".to_owned());
        }

        // Assemble and return the result. Ok(...) means success.
        Ok(SandboxOutput {
            stdout,
            stderr,
            // .code() gets the exit code; None when killed by a signal.
            exit_code: status.code(),
            // Always false here — because timeouts are judged uniformly by the outer Sandbox, and
            // a process that made it here did not time out.
            timed_out: false,
            // Use the label from our own capabilities.
            isolation: self.capabilities().label.to_owned(),
            // Put in the degradation entries collected above.
            degraded,
        })
    }
}
