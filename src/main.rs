use std::format;

use agent_sdk::{Message, tool};
use serde::{Deserialize, Serialize};
use serde_json;

fn main() -> Result<(), serde_json::Error> {
    let messages: Vec<Message> = vec![
        Message::System {
            content: "你是Shirley, 你的最喜欢的人是鲁鲁修".into(),
        },
        Message::User {
            content: "你的名字？你喜欢的人是谁？".into(),
        },
    ];

    println!("{}", serde_json::to_string(&messages)?);

    let message_duplicate =
        serde_json::from_str::<Vec<Message>>(&serde_json::to_string(&messages)?)?;

    println!("{:?}", message_duplicate);

    println!("{}", hello("Shirley".into()));

    Ok(())
}

#[tool(description = "向用户打招呼")]
fn hello(#[param(description = "用户的名字")] name: String) -> String {
    format!("Hello {name}")
}

#[tool(description = "查询城市天气")]
fn get_weather(
    #[param(description = "城市名称，例如北京")] location: String,

    #[param(description = "温度单位，可以省略")] unit: Option<String>,
) -> Result<String, String> {
    Ok(format!("城市：{location}，单位：{unit:?}"))
}
