mod common;

use std::error::Error;

use llm::{
    client::{ProviderClient, completion::CompletionClient},
    completion::CompletionModel as _,
    providers::deepseek,
};

/// Runs the blocking DeepSeek Chat Completions example with thinking mode enabled.
///
/// Required environment variable: DEEPSEEK_API_KEY
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = deepseek::Client::from_env();
    let model = client
        .completion_model(deepseek::DEEPSEEK_PRO)
        .with_thinking(true)
        .with_reasoning_effort("high");

    let response = model
        .completion_request(
            "In one short sentence, explain what the DeepSeek Chat Completions API does.",
        )
        .send()
        .await?;

    println!("text:\n{}", common::assistant_text(&response.choice));
    println!("usage: {:?}", response.usage);

    Ok(())
}
