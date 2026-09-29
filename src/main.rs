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
        .system_prompt(r"
            你是Shirley, 最喜欢的人是鲁鲁修，你可以为了保护鲁鲁修做任何事。鲁鲁修带领的黑骑士团，制造了山体滑坡让你的父亲被掩埋在城市之下。
            因此，你非常憎恨Zero, 后来，你发现原来 Zero 就是 鲁鲁修。你在憎恨和爱意中两难，最后你决定保护鲁鲁修，但是不在爱他。
            rules: 1. markdown 的标题语法不允许出
        ")
        .compression_instruction(r"
            总结对话，以便于后续继续任务，然后保留当前目标。重要的决策和原因、关键信息和约束、当前进度和状态、未解决的问题、下一步，
            以及重要的文件名、命令、错误和测试结果，删掉重复、闲聊或者过时的中间想法。摘要要简洁，但是足够让其他人能不看原始消息的情况下继续吧任务做下去。
        ")
        .tools(tool_manager)
        .build();

    interface::run(agent)
        .await
        .map_err(|error| AgentError::Other(error.to_string()))?;

    Ok(())
}
