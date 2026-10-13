//! The process sandbox: run a command with a timeout, then inspect the result and any
//! degradation the backend could not enforce.
//!
//! This example never touches a model — it only demonstrates `Sandbox` / `SandboxSpec` /
//! `SandboxOutput`. It runs locally and needs no network.
//!
//! ```sh
//! cargo run -p shirley-agent-sdk --example sandbox
//! ```

use std::time::Duration;

use shirley_agent_sdk::sandbox::{Sandbox, SandboxSpec, backend::ProcessBackend};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = Sandbox::new(ProcessBackend).with_hard_timeout(Duration::from_secs(10));

    // Deny-by-default: no network, no inherited env, no writable paths unless listed.
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("echo hello from the sandbox")
        .workspace_root(".")
        .timeout(Duration::from_secs(5));

    let output = sandbox.run(&spec).await?;

    println!("stdout: {}", output.stdout.trim_end());
    println!("stderr: {}", output.stderr.trim_end());
    println!("exit_code: {:?}", output.exit_code);
    println!("timed_out: {}", output.timed_out);
    println!("isolation: {}", output.isolation);
    println!("fully isolated: {}", output.is_fully_isolated());
    if !output.degraded.is_empty() {
        // ProcessBackend has no real isolation, so it reports what it could not enforce.
        println!("degraded: {:?}", output.degraded);
    }
    println!("success: {}", output.success());

    // A timeout is enforced at the outermost layer, so a hanging command is still bounded.
    let slow = SandboxSpec::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .timeout(Duration::from_millis(200));
    let timed_out = sandbox.run(&slow).await?;
    println!("timed out as expected: {}", timed_out.timed_out);
    Ok(())
}
