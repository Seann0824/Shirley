//! The `ContextProvider` seam: append application-owned context to the end of every request.
//!
//! The SDK does not care what the text means (a task ledger, a scratchpad, a constraint). It
//! calls the provider while assembling each request and appends the returned string as a
//! `System` message **at the end** — so it never enters `self.messages`, compaction cannot
//! touch it, and it survives across compaction.
//!
//! Because it is appended at the tail, a stable value keeps the prefix cache intact.

use std::sync::Mutex;

use shirley_agent_sdk::{Agent, ContextProvider, ModelConfig, ModelProtocol};

/// A tiny "task ledger" the application owns. Providers take `&self`, so interior
/// mutability (`Mutex` / atomics) is up to the implementation.
#[derive(Default)]
struct Ledger {
    steps: Mutex<Vec<(String, bool)>>,
}

impl Ledger {
    fn add(&self, step: &str) {
        self.steps.lock().unwrap().push((step.to_owned(), false));
    }
}

impl ContextProvider for Ledger {
    fn context(&self) -> Option<String> {
        let steps = self.steps.lock().unwrap();
        if steps.is_empty() {
            return None;
        }
        let mut out = String::from("<task_state>\n");
        for (step, done) in steps.iter() {
            out.push_str(&format!("- [{}] {step}\n", if *done { "x" } else { " " }));
        }
        out.push_str("</task_state>");
        Some(out)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    let base_url = std::env::var("LOCAL_BASE_URL")?;
    let api_key = std::env::var("LOCAL_API_KEY").ok();
    let model = std::env::var("LOCAL_MODEL").unwrap_or_else(|_| "default-model".into());

    let ledger = std::sync::Arc::new(Ledger::default());
    ledger.add("Write the migration script");
    ledger.add("Run the test suite");

    let model_config = ModelConfig::builder()
        .protocol(ModelProtocol::Responses)
        .base_url(base_url)
        .model(model)
        .maybe_api_key(api_key)
        .build();

    let mut agent = Agent::builder()
        .model_config(model_config)
        .system_prompt("You are a concise assistant.")
        .context_provider(ledger.clone())
        .build()?;

    // The provider's text is appended as a system message on every request, without being
    // stored in the conversation history.
    let result = agent.run("What is the second step in my task list?").await?;
    println!("{:?}", result.messages.last());
    Ok(())
}
