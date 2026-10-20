//! Real-endpoint smoke: drive the same task through a model that *supports* native tool calls
//! (`ToolCallingMode::Native`) and one that *does not* (`ToolCallingMode::Tagged`, the text
//! protocol), printing every `AgentEvent` so we can see exactly what the model emitted and which
//! tool ran.
//!
//! Usage:
//!   LOCAL_BASE_URL=... LOCAL_API_KEY=... \
//!   NATIVE_MODEL=default-model TAGGED_MODEL=default-model \
//!   cargo run -p shirley-agent-sdk --example real_tool_calling

use futures::StreamExt;
use shirley_agent_sdk::{
    Agent, AgentEvent, Message, ModelConfig, ModelProtocol, ToolCallingMode, ToolManager, tool,
};

/// The single tool offered to the model.
#[tool(description = "Return the current weather for a city as a short string")]
async fn get_weather(
    #[param(description = "city name, e.g. Hangzhou")] city: String,
) -> Result<String, shirley_agent_sdk::ToolError> {
    Ok(format!("{city}: 21C, clear (mock data)"))
}

async fn run_one(label: &str, base_url: &str, api_key: &str, model: &str, mode: ToolCallingMode) {
    println!("\n================ {label} (model={model}, mode={mode:?}) ================");
    let mut tools = ToolManager::new();
    tools.register(get_weather::tool()).unwrap();

    let config = ModelConfig::builder()
        .protocol(ModelProtocol::Responses)
        .base_url(base_url)
        .api_key(api_key.to_string())
        .model(model)
        .stream(false)
        .tool_calling(mode)
        .build();

    let mut agent = Agent::builder()
        .model_config(config)
        .system_prompt("You are a concise assistant. Use the provided tools when asked.")
        .tools(tools)
        .build()
        .unwrap();

    let task =
        "Use the get_weather tool to look up the weather in Hangzhou, then tell me the result.";
    let mut stream = agent.run_stream(task);
    while let Some(event) = stream.next().await {
        match event {
            Ok(AgentEvent::ToolStarted {
                name, arguments, ..
            }) => {
                println!("  -> ToolStarted: {name} args={arguments}");
            }
            Ok(AgentEvent::ToolFinished {
                name,
                ok,
                output,
                elapsed_ms,
                ..
            }) => {
                println!("  <- ToolFinished: {name} ok={ok} out={output:?} ({elapsed_ms}ms)");
            }
            Ok(AgentEvent::Finished(result)) => {
                println!("  Finished: {:?}", result.stop_reason);
                for message in &result.messages {
                    match message {
                        Message::Assistant {
                            content,
                            tool_calls,
                            ..
                        } => {
                            println!("  [assistant] content={content:?} tool_calls={tool_calls:?}");
                        }
                        Message::Tool { content, .. } => {
                            println!("  [tool] {content:?}");
                        }
                        other => println!("  [other] {other:?}"),
                    }
                }
            }
            Ok(other) => println!("  . {other}"),
            Err(error) => println!("  !! error: {error}"),
        }
    }
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let base_url = std::env::var("LOCAL_BASE_URL").expect("LOCAL_BASE_URL not set");
    let api_key = std::env::var("LOCAL_API_KEY").unwrap_or_default();
    let native = std::env::var("NATIVE_MODEL").unwrap_or_else(|_| "default-model".into());
    // Same model by default: the point is to compare the two *modes*, not two models.
    let tagged = std::env::var("TAGGED_MODEL").unwrap_or_else(|_| native.clone());

    run_one(
        "NATIVE (supports tool calls)",
        &base_url,
        &api_key,
        &native,
        ToolCallingMode::Native,
    )
    .await;
    run_one(
        "TAGGED (text protocol, no native tool calls)",
        &base_url,
        &api_key,
        &tagged,
        ToolCallingMode::Tagged,
    )
    .await;
}
