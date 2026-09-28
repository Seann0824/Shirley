use std::{collections::HashMap, io::Write, println, todo};

use bon::vec;
use futures::StreamExt;

use super::encode_request;
use crate::{
    ModelConfig, ModelProtocol, ToolManager,
    adapter::{ModelRequest, chat_completions::dto::ModelStreamResponse},
    message,
};

pub async fn test_sse() {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("LOCAL_API_KEY").expect("缺少 APIKEY");
    let base_url = std::env::var("LOCAL_BASE_URL").expect("缺少 BASE URL");
    let model_config = ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url(base_url)
        .api_key(api_key)
        .model("deepseek-v4.1-flash")
        .stream(true)
        .thinking(true)
        .build();
    let tools = ToolManager::new();
    let mode_request = ModelRequest {
        messages: &vec![message::Message::User {
            content: "你是谁".into(),
        }],
        tools: &tools.definitions(),
    };

    let client = reqwest::Client::new();
    let prepared = encode_request(&model_config, &mode_request).unwrap();

    let response = client
        .post(&prepared.url)
        .headers(prepared.headers)
        .json(&prepared.body)
        .send()
        .await
        .unwrap();

    let mut byte_stream = response.bytes_stream();
    let mut buffer = String::new();
    while let Some(chunk) = byte_stream.next().await {
        let chunk = chunk.unwrap();
        let text = String::from_utf8_lossy(&chunk);
        buffer.push_str(&text);

        while let Some(pos) = buffer.find("\n\n") {
            // 我觉得这个json结构应该一样的吧？
            let section = buffer[..pos].to_string();
            // 去掉当前读取后的事件 和 两个换行符号
            buffer.drain(..pos + 2);

            let mut event = String::new();
            let mut data = String::new();
            // 解析每行的数据
            for line in section.lines() {
                if let Some(rest) = line.strip_prefix("event:") {
                    // 一个个sse应该有对应event，但是看起来model好像没有遵循这个规范。
                    event = rest.trim().into();
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data.push_str(rest.trim());
                } else if let Some(rest) = line.strip_prefix("id:") {
                    // todo: 当前event 的id，多用于后续网络抖动重试。不知道模型服务器是否支持，后续我们可以验证一下。
                    todo!()
                }
            }
            // 为啥会出现
            let payload = data.trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let value = serde_json::from_str::<ModelStreamResponse>(&data.trim()).unwrap();
            let choice = &value.choices.get(0).unwrap();
            let delta = &choice.delta;
            let reasoning_content = delta.reasoning_content.clone().unwrap_or(String::new());
            let content = delta.content.clone().unwrap_or(String::new());
            let finish_reason = &choice.finish_reason;

            if reasoning_content.len() > 0 {
                print!("{}", reasoning_content);
                std::io::stdout().flush().unwrap();
            }
            if content.len() > 0 {
                print!("{}", content);
                std::io::stdout().flush().unwrap();
            }

            if let Some(finish_reason) = finish_reason {
                match finish_reason {
                    // "stop"
                    // | "length"
                    // | "content_filter"
                    // | "tool_calls"
                    // | "insufficient_system_resource"
                    // | "aborted" => {}
                    _ => todo!(),
                }
            }
            // 这里必然返回 assistant， 所以接下来我们就是要把数据向外yield
        }

        // event: xxxx
        // id: 1
        // data: xxxx
        /*
           data: xxxx

        */
    }
}

#[cfg(test)]
mod test {
    use std::println;

    use super::test_sse;

    #[tokio::test]
    async fn test_sse_stream() {
        test_sse().await;
    }

    #[test]
    fn test_deserialize() {
        use super::ModelStreamResponse;
    }
}
