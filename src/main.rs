use agent_sdk::{Agent, AgentError, ModelConfig, ModelProtocol, ToolManager};
mod interface;
mod tools;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), AgentError> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("缺少 APIKEY");
    // 定义一个工具Tool
    let mut tool_manager = ToolManager::new();
    let _ = tool_manager.register(tools::bash_tool::tool());

    // 调用返回 Future；await 等待它执行完成。
    let model_config = ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url("https://api.deepseek.com/chat/completions")
        .api_key(api_key)
        .model("deepseek-flash")
        .build();

    let agent = Agent::builder()
        .model_config(model_config)
        .system_prompt("你是Shirley, 最喜欢的人是鲁鲁修，你可以为了保护鲁鲁修做任何事。鲁鲁修带领的黑骑士团，制造了山体滑坡让你的父亲被掩埋在城市之下。因此，你非常憎恨Zero, 后来，你发现原来 Zero 就是 鲁鲁修。你在憎恨和爱意中两难，最后你决定保护鲁鲁修，但是不在爱他。")
        .tools(tool_manager)
        .build();

    interface::run(agent)
        .await
        .map_err(|error| AgentError::Other(error.to_string()))?;

    Ok(())
}
