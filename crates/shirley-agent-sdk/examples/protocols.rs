//! One agent, three wire protocols.
//!
//! Everything downstream of `ModelConfig` is protocol-agnostic: the same `Agent`, `Message`
//! and tool code runs against ChatCompletions, Responses, or Anthropic Messages. `base_url`
//! is the full request endpoint — the adapter POSTs to it as-is.
//!
//! This example just builds the three configs; it does not send a request (that would need
//! real credentials). Swap `.build()` for a run to try it against a live endpoint.

use shirley_agent_sdk::{ModelConfig, ModelProtocol, ToolCallingMode};

fn main() {
    // OpenAI Chat Completions.
    let _chat = ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url("https://api.deepseek.com/v1/chat/completions")
        .api_key("sk-...")
        .model("deepseek-chat")
        .build();

    // OpenAI Responses.
    let _responses = ModelConfig::builder()
        .protocol(ModelProtocol::Responses)
        .base_url("https://api.deepseek.com/responses")
        .api_key("sk-...")
        .model("deepseek-chat")
        .build();

    // Anthropic Messages — the adapter adds `x-api-key` and `anthropic-version` for you.
    let _anthropic = ModelConfig::builder()
        .protocol(ModelProtocol::AnthropicMessages)
        .base_url("https://api.deepseek.com/anthropic/v1/messages")
        .api_key("sk-...")
        .model("deepseek-chat")
        .build();

    // Tool transport mode is orthogonal to the protocol. For a model without native tool
    // calling, switch to the text protocol with one field — the ReAct loop and the
    // `AgentEvent` stream are unchanged.
    let _tagged = ModelConfig::builder()
        .protocol(ModelProtocol::ChatCompletions)
        .base_url("http://localhost:11434/v1/chat/completions")
        .model("local-model")
        .tool_calling(ToolCallingMode::Tagged)
        .build();

    println!("built four ModelConfigs: chat / responses / anthropic / tagged");
}
