mod common;

use std::error::Error;

use futures_util::StreamExt as _;
use llm::{
    completion::{CompletionModel as _, Message},
    providers::openai,
    streaming::StreamedAssistantContent,
    usage::GetTokenUsage as _,
};

/// Runs the streaming OpenAI Responses example.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = common::openai_client()?;
    let model = openai::responses_api::ResponsesCompletionModel::with_model(client, common::MODEL);
    let mut stream = model
        .completion_request(Message::user(
            "Write a short poem about dawn in the city in 8 lines so the streaming output is easy to see.",
        ))
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
