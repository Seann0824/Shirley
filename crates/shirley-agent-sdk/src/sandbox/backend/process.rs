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
use std::process::Stdio;

// tokio's async Command: used to spawn the subprocess (async = does not block while waiting for it
// to finish).
use tokio::process::Command;

// Bring in the interface to implement and the types to use.
use crate::sandbox::backend::{Capabilities, SandboxBackend, SandboxError};
use crate::sandbox::output::SandboxOutput;
use crate::sandbox::spec::{NetworkPolicy, SandboxSpec};

// Define an empty struct (no fields) — it is just a "marker" and needs to store no data.
// derive(Default) lets ProcessBackend::default() create it.
#[derive(Default)]
pub struct ProcessBackend;

// `impl SandboxBackend for ProcessBackend` = "make ProcessBackend satisfy the SandboxBackend
// contract". The braces must implement every method the trait requires.
impl SandboxBackend for ProcessBackend {
    // Implement capabilities: report that it "can isolate nothing".
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            filesystem_isolation: false, // cannot isolate the filesystem
            network_isolation: false,    // cannot isolate the network
            resource_limits: false,      // cannot limit resources
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

        // Spawn and wait for the subprocess to finish, capturing the output.
        let output = cmd
            .output() // wait asynchronously, returning Result<Output, io::Error>
            .await    // .await means "wait for this async operation to complete"
            // If spawning fails, turn io::Error into our SandboxError::Spawn.
            // `?` means "return early if it's Err".
            .map_err(|e| SandboxError::Spawn(format!("failed to spawn {}: {e}", spec.program)))?;

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
            NetworkPolicy::Disabled => {
                degraded.push("no network isolation: NetworkPolicy::Disabled is not enforced".to_owned())
            }
            // If a proxy is required, we likewise cannot do it; say so faithfully.
            // `{ .. }` means "we don't care about the addr value inside; ignore it".
            NetworkPolicy::Proxy { .. } => {
                degraded.push("no network isolation: proxy egress is not enforced".to_owned())
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
            // Convert the subprocess's stdout bytes into a string.
            // from_utf8_lossy: replaces invalid bytes with a placeholder instead of erroring.
            // .into_owned(): turns the borrowed result into an owned String.
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            // .code() gets the exit code; None when killed by a signal.
            exit_code: output.status.code(),
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
