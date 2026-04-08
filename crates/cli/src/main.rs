mod runtime;

use std::{env, io};

use std::sync::Arc;

use kernel::{
    model::LlmAgentModel,
    session::InMemorySessionStore,
    tools::{builtin::default_read_only_tools, registry::ToolRegistry},
};
use llm::providers::openai;
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_MODEL: &str = "gpt-5.4";

/// Reads a required environment variable for the CLI adapter.
fn require_env(name: &str) -> Result<String, io::Error> {
    env::var(name)
        .map_err(|_| io::Error::other(format!("missing {name}; set it before running cli")))
}

/// Prints the CLI usage string expected by the integration test.
fn usage_message() -> &'static str {
    "usage: cargo run -p cli -- \"your prompt\""
}

/// Initializes tracing output for the CLI when `RUST_LOG` is set.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .try_init();
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");
    if prompt.trim().is_empty() {
        info!("cli invoked without prompt");
        eprintln!("{}", usage_message());
        std::process::exit(2);
    }

    let base_url = require_env("OPENAI_BASE_URL")?;
    let api_key = require_env("OPENAI_API_KEY")?;
    let model_name = env::var("OPENAI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());

    info!(base_url = %base_url, model = %model_name, prompt = %runtime::prompt_preview(&prompt), "starting cli request");

    let client = openai::Client::builder()
        .base_url(base_url)
        .api_key(api_key)
        .build()?;
    let llm_model =
        openai::responses_api::ResponsesCompletionModel::with_model(client, &model_name);
    let model = Arc::new(LlmAgentModel::new(llm_model));
    let store = Arc::new(InMemorySessionStore::default());
    let registry = Arc::new(ToolRegistry::default());
    for tool in default_read_only_tools() {
        registry.register_arc(tool).await;
    }

    info!(
        tool_count = registry.definitions().await.len(),
        "registered read-only tools"
    );

    let result = runtime::run_cli_prompt(model, store, registry, prompt).await?;

    println!("{result}");
    Ok(())
}
