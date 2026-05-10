//! Minimal DeepSeek streaming example.
//!
//! Run with:
//! `DEEPSEEK_API_KEY=... cargo run -p provider --example deepseek_stream`

use futures::StreamExt;
use provider::client::{CompletionClient, ProviderClient};
use provider::completion::CompletionModel;
use provider::providers::deepseek::{Client, DEEPSEEK_V4_FLASH};
use serde_json::json;
use std::io::Write;

/// Streams a poetry prompt from DeepSeek and prints text chunks as they arrive.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::from_env()?;
    let model = client.completion_model(DEEPSEEK_V4_FLASH);

    let mut stream = model
        .completion_request(
            "Write a long free-verse poem about a quiet night and a distant river. \
             Make it at least twelve lines, with vivid imagery, recurring river motifs, \
             and a reflective ending.",
        )
        // Enable DeepSeek thinking so the stream includes reasoning chunks.
        .additional_params(json!({
            "reasoning_effort": "max",
            "thinking": { "type": "enabled" }
        }))
        .stream()
        .await?;

    while let Some(item) = stream.next().await {
        match item? {
            provider::streaming::StreamedAssistantContent::Text(text) => {
                print!("{}", text.text);
                std::io::stdout().flush()?;
            }
            provider::streaming::StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
                print!("{}", reasoning);
                std::io::stdout().flush()?;
            }
            provider::streaming::StreamedAssistantContent::Final(_) => {}
            _ => {}
        }
    }

    println!();
    Ok(())
}
