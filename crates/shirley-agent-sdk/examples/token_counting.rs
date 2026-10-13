//! Token accounting: a CJK-aware heuristic estimate used for compaction cut points.
//!
//! It is "good enough, not exact" by design — see `docs/compaction.md` §4. The estimate is
//! calibrated against the real `input_tokens` reported by the provider, so the raw functions
//! here always return the **uncalibrated** value.
//!
//! ```sh
//! cargo run -p shirley-agent-sdk --example token_counting
//! ```

use shirley_agent_sdk::{Message, count_message, count_messages, count_text};

fn main() {
    println!("'hello world' -> {}", count_text("hello world"));
    // CJK is billed at ~1 token per character, deliberately biased high.
    println!("'你好世界'   -> {}", count_text("你好世界"));

    let messages = vec![
        Message::System { content: "You are a concise assistant.".into() },
        Message::User { content: "What is 2 + 3?".into() },
        Message::Assistant {
            content: Some("5".into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
        },
    ];
    println!("one message  -> {}", count_message(&messages[1]));
    println!("whole list   -> {}", count_messages(&messages));
}
