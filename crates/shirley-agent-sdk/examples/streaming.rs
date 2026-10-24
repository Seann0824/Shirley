//! Consume the `AgentEvent` stream: incremental text, reasoning, tool activity, usage.
//!
//! This is what a UI does — it renders deltas as they arrive instead of waiting for the
//! whole turn. `run_stream()` yields `Result<AgentEvent, AgentError>`; the stream ends after
//! `Finished(RunResult)`.
//!
//! ```sh
//! LOCAL_BASE_URL=http://127.0.0.1:8788/v1/responses LOCAL_MODEL=default-model \
//! cargo run -p shirley-agent-sdk --example streaming
//! ```

use futures::StreamExt;
use shirley_agent_sdk::{Agent, AgentEvent, ModelConfig, ModelProtocol};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    let base_url = std::env::var("LOCAL_BASE_URL")?;
    let api_key = std::env::var("LOCAL_API_KEY").ok();
    let model = std::env::var("LOCAL_MODEL").unwrap_or_else(|_| "default-model".into());

    let model_config = ModelConfig::builder()
        .protocol(ModelProtocol::Responses)
        .base_url(base_url)
        .model(model)
        .maybe_api_key(api_key)
        .stream(true) // request SSE streaming
        .build();

    let mut agent = Agent::builder()
        .model_config(model_config)
        .system_prompt("You are a concise assistant.")
        .build()?;

    let mut stream = agent.run_stream("Count from 1 to 5, one number per line.");
    while let Some(event) = stream.next().await {
        match event? {
            AgentEvent::Retrying { attempt, reason } => eprintln!("[retry {attempt}] {reason}"),
            AgentEvent::ReasoningDelta(text) => print!("[reasoning] {text}"),
            AgentEvent::ContentDelta(text) => print!("{text}"),
            AgentEvent::ToolStarted {
                name, arguments, ..
            } => {
                println!("\n-> {name}({arguments})");
            }
            AgentEvent::ToolFinished {
                name, ok, output, ..
            } => {
                println!("<- {name} ok={ok}: {output}");
            }
            AgentEvent::Usage(usage) => {
                println!(
                    "\n[usage] in={} out={}",
                    usage.input_tokens, usage.output_tokens
                );
            }
            AgentEvent::ContextUsage {
                used_tokens,
                limit_tokens,
            } => {
                println!("[context] {used_tokens}/{limit_tokens}");
            }
            AgentEvent::Notice(text) => println!("[notice] {text}"),
            AgentEvent::CompactionStarted => println!("[compaction] started"),
            AgentEvent::CompactionFinished => println!("[compaction] finished"),
            AgentEvent::MessageAdded(_) => {}
            AgentEvent::Finished(result) => {
                println!(
                    "\n[finished] {:?}, {} new messages",
                    result.stop_reason,
                    result.messages.len()
                );
            }
        }
    }
    Ok(())
}
