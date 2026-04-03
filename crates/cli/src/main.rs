use std::{env, io, sync::Arc};

use async_trait::async_trait;
use kernel::{
    events::{AgentEvent, EventSink},
    model::LlmAgentModel,
    runtime::{AgentRunner, RunRequest},
    session::{InMemorySessionStore, SessionId, ThreadId},
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

/// Builds a short prompt preview so tracing stays readable for long requests.
fn prompt_preview(prompt: &str) -> String {
    const MAX_CHARS: usize = 80;
    let mut preview = prompt.trim().replace('\n', " ");
    if preview.chars().count() > MAX_CHARS {
        preview = format!("{}...", preview.chars().take(MAX_CHARS).collect::<String>());
    }
    preview
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

struct TracingEventSink;

#[async_trait]
impl EventSink for TracingEventSink {
    /// Emits runtime events into the CLI tracing stream for interactive debugging.
    async fn publish(&self, event: AgentEvent) {
        match event {
            AgentEvent::RunStarted {
                session_id,
                thread_id,
                input,
            } => {
                info!(session_id, thread_id, prompt = %prompt_preview(&input), "agent run started");
            }
            AgentEvent::ModelRequested {
                message_count,
                tool_count,
            } => {
                info!(message_count, tool_count, "requesting model completion");
            }
            AgentEvent::ToolCallRequested { name, arguments } => {
                info!(tool = %name, arguments = %arguments, "tool requested");
            }
            AgentEvent::ToolCallCompleted { name, output } => {
                info!(tool = %name, output = %output, "tool completed");
            }
            AgentEvent::TextProduced { text } => {
                info!(text = %text, "model produced final text");
            }
            AgentEvent::RunFinished { text, usage } => {
                info!(
                    text = %text,
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    total_tokens = usage.total_tokens,
                    "agent run finished"
                );
            }
        }
    }
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

    info!(base_url = %base_url, model = %model_name, prompt = %prompt_preview(&prompt), "starting cli request");

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

    let runner = AgentRunner::new(model, store, registry, Arc::new(TracingEventSink))
        .with_system_prompt(
            "You are a helpful agent. Use tools when they are useful, and answer directly when they are not.",
        );

    let result = runner
        .run(RunRequest::new(SessionId::new(), ThreadId::new(), prompt))
        .await?;

    println!("{}", result.text);
    Ok(())
}
