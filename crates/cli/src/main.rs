mod config;

use std::{env, fs::OpenOptions, io, sync::Arc};

use config::AppConfig;
use kernel::{model::FactoryLlmAgentModel, session::InMemorySessionStore};
use llm::providers::LlmModelFactory;
use tools::create::create_default_tool_router;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Builds the CLI model adapter selected by the loaded app config.
fn build_agent_model(
    config: &AppConfig,
) -> Result<FactoryLlmAgentModel, Box<dyn std::error::Error>> {
    let factory = LlmModelFactory::try_from_config(config.llm.clone())?;
    let model = factory.completion_model_ref(config.current_model_ref())?;
    Ok(FactoryLlmAgentModel::new(model))
}

/// Selects whether the binary runs as a human ACP client or as the exported ACP stdio agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliMode {
    /// Human-facing interactive CLI implemented as an ACP client.
    Client,
    /// Machine-facing ACP server over stdio.
    Server,
}

/// Parses the CLI mode while keeping every supported path ACP-based.
fn parse_cli_mode(args: impl IntoIterator<Item = String>) -> CliMode {
    match args.into_iter().next().as_deref() {
        Some("serve") => CliMode::Server,
        _ => CliMode::Client,
    }
}

/// Initializes tracing output for the ACP CLI process in `cli.log`.
fn init_tracing() -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("cli.log")?;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(false)
        .without_time()
        .with_writer(log_file)
        .try_init();
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing()?;

    let mode = parse_cli_mode(env::args().skip(1));
    let config = config::app_config();

    info!(
        current_model = %config.current_model_ref(),
        provider_count = config.llm.providers.len(),
        mode = ?mode,
        "starting ACP cli agent"
    );

    let model = Arc::new(build_agent_model(&config)?);
    let store = Arc::new(InMemorySessionStore::default());
    let router = Arc::new(create_default_tool_router().await);
    let skills = config.skills.to_skill_config();

    info!(
        tool_count = router.definitions().await.len(),
        "registered default tools through the extracted tools crate"
    );

    match mode {
        CliMode::Client => {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            acp::run_interactive_cli_via_acp(
                Arc::clone(&model),
                Arc::clone(&store),
                Arc::clone(&router),
                skills,
                &mut input,
                io::stdout(),
            )
            .await?;
        }
        CliMode::Server => {
            acp::run_sdk_stdio_agent(
                Arc::clone(&model),
                Arc::clone(&store),
                Arc::clone(&router),
                skills,
            )
            .await?;
        }
    }

    Ok(())
}
