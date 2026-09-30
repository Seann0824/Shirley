use std::path::PathBuf;
use std::time::Duration;

use agent_sdk::sandbox::{Sandbox, SandboxOutput, SandboxSpec, backend::ProcessBackend};
use agent_sdk::tool;

// 静态拒绝列表：在真正的权限层（PermissionPolicy，见 docs/security.md 第五节）落地前的临时兜底。
// 注意：字符串匹配可被绕过（/bin/rm、r''m、base64 | bash），
// 真正的边界在沙盒后端，不在这个列表里。
const BLACKLIST: [&str; 3] = ["rm", "shutdown", "reboot"];

/// 工作区根目录：优先读环境变量 `SHIRLEY_WORKSPACE`，否则退回当前目录。
///
/// 沙盒会把它作为 cwd，并（在真实后端里）据此限制文件系统可见范围。
fn workspace_root() -> Option<PathBuf> {
    std::env::var("SHIRLEY_WORKSPACE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// 把沙盒结果渲染成模型可读的文本。
///
/// 模型需要看到 stderr 与退出码才能自我纠正（docs/security.md 6.1）；
/// 降级信息则诚实告知"这次隔离没生效"，避免上层误以为命令被约束住了。
fn render(output: &SandboxOutput, command: &str) -> String {
    let mut text = String::new();

    if output.timed_out {
        text.push_str(&format!("[超时] 命令执行超时: {command}\n"));
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

    if let Some(code) = output.exit_code {
        if code != 0 {
            text.push_str(&format!("[exit code] {code}\n"));
        }
    }

    // // 降级提示：本次执行的隔离并未完全生效。诚实上报，不静默忽略。
    // if !output.is_fully_isolated() {
    //     text.push_str(&format!(
    //         "[沙盒降级] 隔离强度: {}，以下约束未生效: {}\n",
    //         output.isolation,
    //         output.degraded.join("；")
    //     ));
    // }

    text
}

// TODO: 工具错误消息格式应该有一个统一的格式，这样就能够按照设计好的格式，去反馈为什么执行失败了。
#[tool(description = "bash 用于执行命令，比如，python、grep、find、git等系统命令")]
pub async fn bash(
    #[param(description = "要执行的命令")] command: String,
    #[param(description = "超时时间（秒）")] timeout: Option<u64>,
) -> Result<String, agent_sdk::ToolError> {
    // 静态兜底拒绝：明显破坏性命令直接拦下。
    // 这层是临时的，真正的边界在下面的沙盒。
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(agent_sdk::ToolError::ExecutionError(
            "命令不能为空".to_string(),
        ));
    }
    if tokens.iter().any(|arg| BLACKLIST.contains(arg)) {
        return Err(agent_sdk::ToolError::ExecutionError(format!(
            "命令包含黑名单命令: {BLACKLIST:?}"
        )));
    }

    // 1. 把命令放进进程沙盒。
    //    保留 `bash -c`（否则管道、重定向等正常用法会坏掉），
    //    但边界改由沙盒后端在"能碰什么"这一层施加，而非在命令字符串上做模式匹配。
    let mut spec = SandboxSpec::new("bash")
        .arg("-c")
        .arg(&command)
        .timeout(Duration::from_secs(timeout.unwrap_or(60)));

    if let Some(root) = workspace_root() {
        spec = spec.workspace_root(root);
    }

    // TODO: 后端应可配置（macOS sandbox-exec / Linux bwrap）。
    // 目前 SandboxBackend::execute 返回 impl Future，trait 非 object-safe，
    // 无法用 Box<dyn> 动态注入，故直接实例化。换后端时改这一行即可。
    let sandbox = Sandbox::new(ProcessBackend::default());

    // 2. 执行。超时由 Sandbox 统一施加，这里不再自己包 tokio::time::timeout。
    let output = sandbox.run(&spec).await.map_err(|e| {
        agent_sdk::ToolError::ExecutionError(format!("{command}: 沙盒执行失败: {e}"))
    })?;

    // 3. 渲染结果返回给模型。
    Ok(render(&output, &command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_simple_command() {
        let out = bash("echo hello".to_string(), Some(10)).await.unwrap();
        assert!(out.contains("hello"), "输出应包含 hello，实际: {out}");
    }

    #[tokio::test]
    async fn surfaces_stderr_and_exit_code() {
        // 失败命令必须把 stderr 与退出码带回来，模型才能自我纠正。
        let out = bash("echo boom >&2; exit 3".to_string(), Some(10))
            .await
            .unwrap();
        assert!(out.contains("boom"), "应包含 stderr: {out}");
        assert!(out.contains("[exit code] 3"), "应包含退出码: {out}");
    }

    #[tokio::test]
    async fn reports_sandbox_degradation() {
        // 当前是 ProcessBackend，隔离未生效，必须诚实上报降级。
        let out = bash("true".to_string(), Some(10)).await.unwrap();
        assert!(out.contains("[沙盒降级]"), "应上报降级: {out}");
    }

    #[tokio::test]
    async fn rejects_blacklisted_command() {
        let err = bash("rm -rf /".to_string(), Some(10)).await.unwrap_err();
        assert!(err.to_string().contains("黑名单"), "应被拦截: {err}");
    }

    #[tokio::test]
    async fn enforces_timeout() {
        let out = bash("sleep 5".to_string(), Some(1)).await.unwrap();
        assert!(out.contains("[超时]"), "应标记超时: {out}");
    }

    #[tokio::test]
    async fn rejects_empty_command() {
        let err = bash("   ".to_string(), Some(10)).await.unwrap_err();
        assert!(err.to_string().contains("不能为空"), "空命令应报错: {err}");
    }
}
