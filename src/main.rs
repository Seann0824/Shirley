use agent_sdk::{Agent, AgentError, ModelConfig, ModelProtocol, ToolManager};
mod interface;
mod prompt;
mod tools;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), AgentError> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("LOCAL_API_KEY").expect("缺少 APIKEY");
    let base_url = std::env::var("LOCAL_BASE_URL").expect("缺少 BASE URL");
    // 定义一个工具Tool
    let mut tool_manager = ToolManager::new();
    let _ = tool_manager.register(tools::bash_tool::tool());

    // 调用返回 Future；await 等待它执行完成。
    let mut model_config = ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url(base_url)
        .api_key(api_key)
        .model("deepseek-v4.1-flash")
        .stream(true)
        .thinking(true)
        .reasoning_effort("low")
        .build();
    model_config.context_window_tokens = Some(104858 >> 1);
    if let Ok(value) = std::env::var("LOCAL_CONTEXT_WINDOW_TOKENS") {
        let limit = value
            .parse::<u64>()
            .map_err(|_| AgentError::Other("LOCAL_CONTEXT_WINDOW_TOKENS 必须是正整数".into()))?;
        if limit == 0 {
            return Err(AgentError::Other(
                "LOCAL_CONTEXT_WINDOW_TOKENS 必须大于 0".into(),
            ));
        }
        model_config.context_window_tokens = Some(limit);
    }
    // 1. 项目工作空间：作为 coding agent 的工作范围根。
    // 2. 系统提示词：角色 + 工作目录 + 项目 Agent.md（由 `prompt` 模块动态拼装）。
    let working_dir = prompt::workspace_root();

    let agent = Agent::builder()
        .model_config(model_config)
        // 提示词以函数形式传入：每次解析都读取当前工作目录与项目 Agent.md，
        // 这样压缩重建后重新置顶的系统提示词始终反映最新的项目状态。
        .system_prompt(prompt::build(working_dir.clone()))
        .working_dir(working_dir)
        // 摘要的取舍口径：coding agent 关心文件 / 命令 / 报错 / 测试结果。
        // 结构与"不得推演、不得编下一步"等硬规则由 SDK 的 `COMPACTION_TEMPLATE` 追加，
        // 这里只写领域相关的偏好（见 `docs/compaction.md` 5.3）。
        .compression_instruction(
            r"
            你在为 coding agent 压缩对话上下文。请忠实保留用户下达的原始指令与约束，
            以及继续任务所必需的信息：关键决策及其原因、当前进度与状态、未解决的问题、
            涉及的文件路径、执行过的命令、遇到的错误和测试结果。删除重复、闲聊与过时的中间想法。
        ",
        )
        .tools(tool_manager)
        .build();

    interface::run(agent)
        .await
        .map_err(|error| AgentError::Other(error.to_string()))?;

    Ok(())
}
