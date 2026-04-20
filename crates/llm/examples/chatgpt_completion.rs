#[allow(dead_code)]
mod common;

use std::error::Error;

use llm::{
    client::{ProviderClient, completion::CompletionClient},
    completion::CompletionModel as _,
    providers::chatgpt,
};

/// Runs one blocking ChatGPT provider completion using either `CHATGPT_ACCESS_TOKEN` or OAuth session auth.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = chatgpt::Client::from_env();
    let model = client.completion_model(chatgpt::GPT_5_3_CODEX);
    let response = model
        .completion_request("In one short sentence, explain what the ChatGPT provider does.")
        .send()
        .await?;

    println!("text:\n{}", common::assistant_text(&response.choice));
    println!("usage: {:?}", response.usage);

    Ok(())
}
