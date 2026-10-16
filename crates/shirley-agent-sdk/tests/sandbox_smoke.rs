//! Sandbox pipeline smoke tests: verify the whole spec -> backend -> output chain runs.
//! Note: this uses ProcessBackend (no isolation); it only verifies the orchestration logic, not
//! the isolation strength.

use shirley_agent_sdk::sandbox::{NetworkPolicy, Sandbox, SandboxSpec, backend::ProcessBackend};
use std::time::Duration;

#[tokio::test]
async fn runs_and_captures_stdout() {
    let sandbox = Sandbox::new(ProcessBackend);
    let spec = SandboxSpec::new("sh").arg("-c").arg("printf hello");
    let out = sandbox.run(&spec).await.unwrap();
    assert_eq!(out.stdout, "hello");
    assert!(out.success());
    // The bare-process backend must faithfully report degradation.
    assert!(!out.is_fully_isolated());
}

#[tokio::test]
async fn captures_stderr_and_exit_code() {
    let sandbox = Sandbox::new(ProcessBackend);
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("echo boom >&2; exit 3");
    let out = sandbox.run(&spec).await.unwrap();
    assert_eq!(out.exit_code, Some(3));
    assert!(out.stderr.contains("boom"));
    assert!(!out.success());
}

#[tokio::test]
async fn caps_single_line_output_and_drains_both_pipes() {
    let sandbox = Sandbox::new(ProcessBackend);
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("head -c 18000000 /dev/zero | tr '\\000' x & head -c 18000000 /dev/zero | tr '\\000' y >&2 & wait; exit 7")
        .timeout(Duration::from_secs(10));
    let out = sandbox.run(&spec).await.unwrap();
    assert!(!out.timed_out, "both pipes must be drained after the cap");
    assert_eq!(out.exit_code, Some(7));
    for (text, byte) in [(&out.stdout, 'x'), (&out.stderr, 'y')] {
        assert!(text.starts_with(&byte.to_string().repeat(32 * 1024)));
        assert!(text.contains("17967232 bytes omitted"));
        assert!(text.len() < 33 * 1024);
    }
}

#[tokio::test]
async fn exact_capture_limit_does_not_report_truncation() {
    let sandbox = Sandbox::new(ProcessBackend);
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("head -c 32768 /dev/zero | tr '\\000' x");
    let out = sandbox.run(&spec).await.unwrap();
    assert!(out.success());
    assert_eq!(out.stdout, "x".repeat(32 * 1024));
    assert!(out.stderr.is_empty());
}

#[tokio::test]
async fn capture_handles_utf8_split_at_the_limit() {
    let sandbox = Sandbox::new(ProcessBackend);
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("head -c 32767 /dev/zero | tr '\\000' x; printf '\\344\\270\\255'");
    let out = sandbox.run(&spec).await.unwrap();
    assert!(out.success());
    assert!(out.stdout.starts_with(&"x".repeat(32767)));
    assert!(out.stdout.contains("2 bytes omitted"));
    assert!(out.stdout.len() < 33 * 1024);
}

#[tokio::test]
async fn enforces_timeout() {
    let sandbox = Sandbox::new(ProcessBackend);
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("sleep 5")
        .timeout(Duration::from_millis(200));
    let out = sandbox.run(&spec).await.unwrap();
    assert!(out.timed_out);
    assert_eq!(out.exit_code, None);
}

#[tokio::test]
async fn env_not_inherited_by_default() {
    // The host sets a variable; it should not be visible inside the sandbox (unless injected explicitly).
    unsafe { std::env::set_var("SANDBOX_LEAK_TEST", "secret") };
    let sandbox = Sandbox::new(ProcessBackend);
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("printf '%s' \"$SANDBOX_LEAK_TEST\"");
    let out = sandbox.run(&spec).await.unwrap();
    assert_eq!(out.stdout, "");
}

#[tokio::test]
async fn env_explicitly_injected_is_visible() {
    let sandbox = Sandbox::new(ProcessBackend);
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("printf '%s' \"$GREETING\"")
        .env("GREETING", "hi");
    let out = sandbox.run(&spec).await.unwrap();
    assert_eq!(out.stdout, "hi");
}

#[tokio::test]
async fn proxy_policy_reported_as_degraded() {
    let sandbox = Sandbox::new(ProcessBackend);
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("true")
        .network(NetworkPolicy::Proxy {
            addr: "127.0.0.1:8888".into(),
        });
    let out = sandbox.run(&spec).await.unwrap();
    assert!(out.degraded.iter().any(|d| d.contains("proxy")));
}
