mod common;

use std::error::Error;

use futures_util::StreamExt as _;
use llm::{
    client::{ProviderClient, completion::CompletionClient},
    completion::CompletionModel as _,
    providers::deepseek,
    streaming::StreamedAssistantContent,
    usage::GetTokenUsage as _,
};

/// Runs the streaming DeepSeek Chat Completions example.
///
/// Required environment variable: DEEPSEEK_API_KEY
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = deepseek::Client::from_env();
    let model = client
        .completion_model(deepseek::DEEPSEEK_PRO)
        .with_thinking(true)
        .with_reasoning_effort("high");

    let mut stream = model
        .completion_request(
            "Write a short poem about moonlight on the sea in 8 lines so the streaming output is easy to see.",
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
