use agent_sdk::tool;
use tokio::process::Command;

const BLACKLIST: [&str; 3] = ["rm", "shutdown", "reboot"];

// TODO: 工具错误消息格式应该有一个统一的格式，这样就能够按照设计好的格式，去反馈为什么执行失败了。
#[tool(description = "在 bash 中执行命令")]
pub async fn bash_tool(
    #[param(description = "要执行的命令")] command: String,
    #[param(description = "超时时间（秒）")] timeout: Option<u64>,
) -> Result<String, agent_sdk::ToolError> {
    // 1. 命令放进进程沙箱, 先忽略这步
    let mut cmd = Command::new("bash");
    // arg 只有一个参数能通过吧？所以应该用args吧？command 是一个字符串，可能包含多个参数，所以需要拆分成多个参数传递给 bash。
    let args: Vec<&str> = command.split_whitespace().collect();

    if args.is_empty() {
        return Err(agent_sdk::ToolError::ExecutionError(
            "命令不能为空".to_string(),
        ));
    }
    let is_blacklisted = args.iter().any(|arg| BLACKLIST.contains(arg));
    if is_blacklisted {
        return Err(agent_sdk::ToolError::ExecutionError(format!(
            "命令包含黑名单命令: {:?}",
            BLACKLIST
        )));
    }

    let timeout = timeout.unwrap_or(60);
    cmd.arg("-c").arg(&command).kill_on_drop(true);

    let output = tokio::time::timeout(std::time::Duration::from_secs(timeout), cmd.output())
        .await
        .map_err(|_| agent_sdk::ToolError::ExecutionError(format!("{}: 命令执行超时", command)))?
        .map_err(|e| {
            agent_sdk::ToolError::ExecutionError(format!("{}: 命令执行失败: {}", command, e))
        })?;

    Ok(String::from_utf8_lossy(&output.stdout).into_owned().into())
}
