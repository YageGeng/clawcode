//! Minimal factory streaming example.
//!
//! Run with:
//! `cargo run -p provider --example factory_say_hi_stream -- <provider_id> <model_id>`

use std::io::{self, Write};

use futures::StreamExt;
use provider::completion::CompletionRequest;
use provider::factory::{LlmFactory, LlmStreamEvent};
use provider::message::Message;

/// Loads `claw.toml`, builds the factory cache, selects one provider/model,
/// and streams a short "say hi" response.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "deepseek".to_string());
    let model_id = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "deepseek-v4-flash".to_string());

    let config = config::load()?;
    let factory = LlmFactory::new(config);
    let llm = factory.get(&provider_id, &model_id).ok_or_else(|| {
        format!("provider/model not found or failed to build: {provider_id}/{model_id}")
    })?;

    // Build a minimal provider-agnostic request with one user message.
    let request = CompletionRequest {
        model: None,
        preamble: None,
        chat_history: provider::OneOrMany::one(Message::user("Say hi in one short sentence.")),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    };

    println!("provider={} model={}", llm.provider_id(), llm.model_id());

    let mut stream = llm.stream(request).await?;
    while let Some(item) = stream.next().await {
        match item? {
            LlmStreamEvent::Text(text) => {
                print!("{}", text.text);
                io::stdout().flush()?;
            }
            LlmStreamEvent::ReasoningDelta { reasoning, .. } => {
                print!("{reasoning}");
                io::stdout().flush()?;
            }
            LlmStreamEvent::Reasoning(reasoning) => {
                print!("{reasoning:?}");
                io::stdout().flush()?;
            }
            LlmStreamEvent::ToolCall { tool_call, .. } => {
                print!("\n[tool_call] {tool_call:?}");
                io::stdout().flush()?;
            }
            LlmStreamEvent::ToolCallDelta { content, .. } => {
                print!("\n[tool_call_delta] {content:?}");
                io::stdout().flush()?;
            }
            LlmStreamEvent::Final { usage, .. } => {
                println!("\n[final usage] {usage:?}");
            }
        }
    }

    Ok(())
}
