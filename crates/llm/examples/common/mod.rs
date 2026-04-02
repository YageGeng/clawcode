use std::{env, error::Error, io};

use llm::{completion::AssistantContent, one_or_many::OneOrMany, providers::openai};

pub const MODEL: &str = "gpt-5.4";

/// Reads a required environment variable for the OpenAI examples.
pub fn require_env(name: &str) -> Result<String, io::Error> {
    env::var(name).map_err(|_| {
        io::Error::other(format!(
            "missing {name}; set it before running this example"
        ))
    })
}

/// Builds an OpenAI client from the shared example environment variables.
pub fn openai_client() -> Result<openai::Client, Box<dyn Error>> {
    let base_url = require_env("OPENAI_BASE_URL")?;
    let api_key = require_env("OPENAI_API_KEY")?;

    Ok(openai::Client::builder()
        .base_url(base_url)
        .api_key(api_key)
        .build()?)
}

/// Flattens assistant text content into a printable string for terminal output.
pub fn assistant_text(choice: &OneOrMany<AssistantContent>) -> String {
    choice
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
