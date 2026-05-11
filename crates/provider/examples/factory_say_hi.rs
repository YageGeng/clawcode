//! Minimal factory example.
//!
//! Run with:
//! `cargo run -p provider --example factory_say_hi -- <provider_id> <model_id>`

use provider::completion::CompletionRequest;
use provider::factory::LlmFactory;
use provider::message::Message;

/// Loads `claw.toml`, builds the factory cache, selects one provider/model,
/// and asks the model to say hi.
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
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    };

    let response = llm.completion(request).await?;
    println!(
        "provider={} model={} message_id={:?}",
        llm.provider_id(),
        llm.model_id(),
        response.message_id
    );

    println!("{}", serde_json::to_string(&response.choice)?);

    Ok(())
}
