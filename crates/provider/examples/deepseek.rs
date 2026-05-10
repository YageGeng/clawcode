//! Minimal DeepSeek example.
//!
//! Run with:
//! `DEEPSEEK_API_KEY=... cargo run -p provider --example deepseek`

use provider::client::{CompletionClient, ProviderClient};
use provider::completion::{AssistantContent, CompletionModel};
use provider::providers::deepseek::{Client, DEEPSEEK_V4_FLASH};
use serde_json::json;

/// Sends a poetry prompt to DeepSeek and prints the model response.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::from_env()?;
    let model = client.completion_model(DEEPSEEK_V4_FLASH);

    let response = model
        .completion_request(
            "Write a lyrical poem about a quiet night and a distant river. \
             Use at least three stanzas and rich sensory imagery.",
        )
        // DeepSeek uses provider-specific reasoning flags to expose thinking output.
        .additional_params(json!({
            "reasoning_effort": "max",
            "thinking": { "type": "enabled" }
        }))
        .send()
        .await?;

    match response.choice.first() {
        AssistantContent::Text(text) => println!("{}", text.text),
        other => println!("{other:?}"),
    }

    Ok(())
}
