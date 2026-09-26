use std::println;

use agent_sdk::{Agent, AgentError, ModelConfig, ModelProtocol, ToolError, ToolManager, tool};
use futures::StreamExt;
mod tools;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), AgentError> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("缺少 APIKEY");
    // 定义一个工具Tool
    let mut tool_manager = ToolManager::new();
    let _ = tool_manager.register(get_current_time::tool());
    let _ = tool_manager.register(tools::bash_tool::tool());

    // 调用返回 Future；await 等待它执行完成。
    let model_config = ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url("https://api.deepseek.com/chat/completions")
        .api_key(api_key)
        .model("deepseek-flash")
        .build();

    let mut agent = Agent::builder()
        .model_config(model_config)
        .system_prompt("你是Shirley, 最喜欢的人是鲁鲁修，你可以为了保护鲁鲁修做任何事。鲁鲁修带领的黑骑士团，制造了山体滑坡让你的父亲被掩埋在城市之下。因此，你非常憎恨Zero, 后来，你发现原来 Zero 就是 鲁鲁修。你在憎恨和爱意中两难，最后你决定保护鲁鲁修，但是不在爱他。")
        .tools(tool_manager)
        .build();

    for task in ["我叫Sean, 你有什么工具列表？"] {
        let mut events = agent.run_stream(task);
        while let Some(event) = events.next().await {
            let event = event?;
            if let agent_sdk::AgentEvent::Finished(_) = &event {
                continue;
            }
            println!("{}", event);
        }
    }

    Ok(())
}

#[tool(description = "向用户打招呼")]
async fn hello(
    #[param(description = "用户的名字")] name: String
) -> Result<String, ToolError> {
    Ok(format!("Hello {name}"))
}

#[derive(serde::Deserialize, schemars::JsonSchema, Debug)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
enum TemperatureUnit {
    Celsius,
    Fahrenheit,
}

#[tool(description = "查询城市天气")]
async fn get_weather(
    #[param(description = "城市名称，例如北京")] location: String,

    #[param(description = "温度单位，可以省略")] unit: Option<TemperatureUnit>,
) -> Result<String, ToolError> {
    Ok(format!("{location} 今天温度是 400F"))
}

// 获取当前时间
#[tool(description = "获取当前时间")]
async fn get_current_time() -> Result<String, ToolError> {
    let now = chrono::Local::now();
    Ok(now.format("%Y-%m-%d %H:%M:%S").to_string())
}
