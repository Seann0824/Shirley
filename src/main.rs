use std::println;

use agent_sdk::{Agent, Message, ModelConfig, ModelProtocol, ToolError, ToolManager, tool};
use serde_json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("缺少 APIKEY");

    let messages: Vec<Message> = vec![
        Message::System {
            content: "你是Shirley, 你的最喜欢的人是鲁鲁修".into(),
        },
        Message::User {
            content: "你的名字？你喜欢的人是谁？".into(),
        },
    ];

    // 定义一个工具Tool
    let mut tool_manager = ToolManager::new();
    let _ = tool_manager.register(get_weather::tool());
    let _ = tool_manager.register(hello::tool());

    // 按你当前管理器接收的格式构造工具调用。
    let call = serde_json::json!({
        "name": "get_weather",
        "arguments": "{\"location\":\"北京\"}"
    });

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
    let on_event = |event: agent_sdk::AgentEvent| {
        println!("{:?}", event);
    };
    let answer = agent.run("我叫Sean, 你叫什么名字", on_event).await?;
    println!("{:?}", answer.messages);
    let answer = agent.run("北京天气", on_event).await?;
    println!("{:?}", answer.messages);
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
