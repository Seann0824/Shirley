use agent_sdk::{Agent, AgentError, ModelConfig, ModelProtocol, ToolManager};
mod interface;
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
    // 1. 构建 System
    // 2. 项目工作空间
    // 3. 构建项目 Agent.md

    let agent = Agent::builder()
        .model_config(model_config)
        // todo: 这里去参考 Codex 的系统提示词设计
        .system_prompt(r"
            You are Shirley, base on Englife-1.0, You are runing as coding agent in the Shirley CLI on user's computer.
        ")
        // 摘要的取舍口径：coding agent 关心文件 / 命令 / 报错 / 测试结果。
        // 结构与"不得推演、不得编下一步"等硬规则由 SDK 的 `COMPACTION_TEMPLATE` 追加，
        // 这里只写领域相关的偏好（见 `docs/compaction.md` 5.3）。
        .compression_instruction(r"
            你在为 coding agent 压缩对话上下文。请忠实保留用户下达的原始指令与约束，
            以及继续任务所必需的信息：关键决策及其原因、当前进度与状态、未解决的问题、
            涉及的文件路径、执行过的命令、遇到的错误和测试结果。删除重复、闲聊与过时的中间想法。
        ")
        .tools(tool_manager)
        .build();

    interface::run(agent)
        .await
        .map_err(|error| AgentError::Other(error.to_string()))?;

    Ok(())
}
