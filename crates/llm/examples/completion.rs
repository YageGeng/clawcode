mod common;

use std::error::Error;

use llm::{
    completion::{CompletionModel as _, Message},
    providers::openai,
};

/// Runs the blocking OpenAI Chat Completions example.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = common::openai_client()?;
    let model =
        openai::completion::CompletionModel::with_model(client.completions_api(), common::MODEL);
    let response = model
        .completion_request(Message::user(
            "In one short sentence, explain what the OpenAI Chat Completions API does.",
        ))
        .send()
        .await?;

    println!("text:\n{}", common::assistant_text(&response.choice));
    println!("usage: {:?}", response.usage);

    Ok(())
}
