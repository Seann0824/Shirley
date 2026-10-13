//! The smallest end-to-end run: one tool, one turn.
//!
//! Set the endpoint via the environment (see the SDK README), then:
//!
//! ```sh
//! LOCAL_BASE_URL=http://127.0.0.1:8788/v1/responses \
//! LOCAL_API_KEY=... LOCAL_MODEL=default-model \
//! cargo run -p shirley-agent-sdk --example quickstart
//! ```

use shirley_agent_sdk::{Agent, ModelConfig, ModelProtocol, ToolError, ToolManager, tool};

/// Define a tool with the `#[tool]` macro. Every parameter needs a description; the JSON
/// Schema is generated from the signature.
#[tool(description = "Add two integers")]
async fn add(
    #[param(description = "Left operand")] left: i64,
    #[param(description = "Right operand")] right: i64,
) -> Result<i64, ToolError> {
    Ok(left + right)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    let base_url = std::env::var("LOCAL_BASE_URL")?;
    let api_key = std::env::var("LOCAL_API_KEY").ok();
    let model = std::env::var("LOCAL_MODEL").unwrap_or_else(|_| "default-model".into());

    let model_config = ModelConfig::builder()
        .protocol(ModelProtocol::Responses)
        // base_url is the full request endpoint, not a base path.
        .base_url(base_url)
        .model(model)
        .maybe_api_key(api_key)
        .stream(false)
        .build();

    let mut tools = ToolManager::new();
    tools.register(add::tool())?;

    let mut agent = Agent::builder()
        .model_config(model_config)
        .system_prompt("You are a concise assistant. Use the provided tools when asked.")
        .tools(tools)
        .build()?;

    // run() blocks until the turn finishes and returns a RunResult.
    let result = agent.run("What is 2 + 3?").await?;
    for message in &result.messages {
        println!("{message}");
    }
    Ok(())
}
