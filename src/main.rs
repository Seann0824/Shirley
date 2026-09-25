use std::format;

use agent_sdk::{Message, ToolError, ToolManager, tool};
use serde::{Deserialize, Serialize};
use serde_json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
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
    let result = tool_manager.invoke(call).await?;
    println!("{result}");

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
