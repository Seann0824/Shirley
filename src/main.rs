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

#[tool]
fn hello(name: String) -> String {
    format!("Hello {name}")
}
