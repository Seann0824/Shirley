//! 沙盒链路冒烟测试：验证 spec → backend → output 全链路可跑。
//! 注意：这里用 ProcessBackend（无隔离），只验证编排逻辑，不验证隔离强度。

use agent_sdk::sandbox::{backend::ProcessBackend, NetworkPolicy, Sandbox, SandboxSpec};
use std::time::Duration;

#[tokio::test]
async fn runs_and_captures_stdout() {
    let sandbox = Sandbox::new(ProcessBackend::default());
    let spec = SandboxSpec::new("sh").arg("-c").arg("printf hello");
    let out = sandbox.run(&spec).await.unwrap();
    assert_eq!(out.stdout, "hello");
    assert!(out.success());
    // 裸进程后端必须如实上报降级。
    assert!(!out.is_fully_isolated());
}

#[tokio::test]
async fn captures_stderr_and_exit_code() {
    let sandbox = Sandbox::new(ProcessBackend::default());
    let spec = SandboxSpec::new("sh").arg("-c").arg("echo boom >&2; exit 3");
    let out = sandbox.run(&spec).await.unwrap();
    assert_eq!(out.exit_code, Some(3));
    assert!(out.stderr.contains("boom"));
    assert!(!out.success());
}

#[tokio::test]
async fn enforces_timeout() {
    let sandbox = Sandbox::new(ProcessBackend::default());
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
    // 宿主设一个变量，沙盒内不应看到（除非显式注入）。
    unsafe { std::env::set_var("SANDBOX_LEAK_TEST", "secret") };
    let sandbox = Sandbox::new(ProcessBackend::default());
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("printf '%s' \"$SANDBOX_LEAK_TEST\"");
    let out = sandbox.run(&spec).await.unwrap();
    assert_eq!(out.stdout, "");
}

#[tokio::test]
async fn env_explicitly_injected_is_visible() {
    let sandbox = Sandbox::new(ProcessBackend::default());
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("printf '%s' \"$GREETING\"")
        .env("GREETING", "hi");
    let out = sandbox.run(&spec).await.unwrap();
    assert_eq!(out.stdout, "hi");
}

#[tokio::test]
async fn proxy_policy_reported_as_degraded() {
    let sandbox = Sandbox::new(ProcessBackend::default());
    let spec = SandboxSpec::new("sh")
        .arg("-c")
        .arg("true")
        .network(NetworkPolicy::Proxy {
            addr: "127.0.0.1:8888".into(),
        });
    let out = sandbox.run(&spec).await.unwrap();
    assert!(out.degraded.iter().any(|d| d.contains("代理")));
}
