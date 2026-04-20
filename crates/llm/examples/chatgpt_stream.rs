#[allow(dead_code)]
mod common;

use std::error::Error;

use futures_util::StreamExt as _;
use llm::{
    client::{ProviderClient, completion::CompletionClient},
    completion::CompletionModel as _,
    providers::chatgpt,
    streaming::StreamedAssistantContent,
    usage::GetTokenUsage as _,
};

/// Runs one streaming ChatGPT provider request and prints text deltas as they arrive.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = chatgpt::Client::from_env();
    let model = client.completion_model(chatgpt::GPT_5_3_CODEX);
    let mut stream = model
        .completion_request(
            "Write three short bullet points about why ChatGPT streaming is useful.",
        )
        .stream()
        .await?;

    while let Some(item) = stream.next().await {
        if let StreamedAssistantContent::Text(text) = item? {
            print!("{}", text.text);
        }
    }

    println!(
        "\n\nfinal text:\n{}",
        common::assistant_text(&stream.choice)
    );
    println!(
        "usage: {:?}",
        stream
            .response
            .as_ref()
            .and_then(|response| response.token_usage())
    );

    Ok(())
}
