mod config;

use std::{env, fs::OpenOptions, io, sync::Arc};

use config::AppConfig;
use kernel::{model::FactoryLlmAgentModel, session::InMemorySessionStore};
use llm::providers::LlmModelFactory;
use store::JsonlSessionStore;
use tools::ToolRouter;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Builds the CLI model adapter selected by the loaded app config.
fn build_agent_model(
    config: &AppConfig,
) -> Result<FactoryLlmAgentModel, Box<dyn std::error::Error>> {
    let factory = LlmModelFactory::try_from_config(config.llm.clone())?;
    let model = factory.completion_model_ref(config.current_model_ref())?;
    Ok(FactoryLlmAgentModel::new(model))
}

/// CLI argument payload parsed from the command line.
struct CliArgs {
    mode: CliMode,
    list_sessions: bool,
    resume_session_id: Option<String>,
    no_persist: bool,
}

/// Parses CLI arguments including interactive flags.
fn parse_args(args: impl IntoIterator<Item = String>) -> CliArgs {
    let mut mode = CliMode::Client;
    let mut list_sessions = false;
    let mut resume_session_id = None;
    let mut resume_requested = false;
    let mut no_persist = false;
    let mut args_iter = args.into_iter();

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "serve" => mode = CliMode::Server,
            "--list-sessions" | "-l" => list_sessions = true,
            "--no-persist" => no_persist = true,
            "--resume" | "-r" => {
                resume_requested = true;
                resume_session_id = args_iter.next();
            }
            other if other.starts_with('-') => {
                eprintln!("warning: unrecognized flag `{other}`");
            }
            _ => {}
        }
    }

    if resume_requested && resume_session_id.is_none() {
        eprintln!("error: --resume requires a session ID argument");
        std::process::exit(1);
    }

    CliArgs {
        mode,
        list_sessions,
        resume_session_id,
        no_persist,
    }
}

/// Selects whether the binary runs as a human ACP client or as the exported ACP stdio agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliMode {
    /// Human-facing interactive CLI implemented as an ACP client.
    Client,
    /// Machine-facing ACP server over stdio.
    Server,
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

    let cli_args = parse_args(env::args().skip(1));
    let config = config::app_config();

    // Handle --list-sessions: print and exit.
    if cli_args.list_sessions {
        match store::list_sessions() {
            Ok(sessions) if sessions.is_empty() => {
                println!("No persisted sessions found.");
            }
            Ok(sessions) => {
                println!("Persisted sessions (newest first):");
                for s in &sessions {
                    println!(
                        "  {}  |  {}  |  {} turns  |  {}",
                        s.id,
                        s.created_at.format("%Y-%m-%d %H:%M"),
                        s.turn_count,
                        s.path.display(),
                    );
                }
            }
            Err(e) => eprintln!("Failed to list sessions: {e}"),
        }
        return Ok(());
    }

    info!(
        current_model = %config.current_model_ref(),
        provider_count = config.llm.providers.len(),
        mode = ?cli_args.mode,
        "starting ACP cli agent"
    );

    let model = Arc::new(build_agent_model(&config)?);
    let store = {
        let builder = InMemorySessionStore::default();
        let persistence_enabled = config.persistence.enabled && !cli_args.no_persist;
        let builder = if persistence_enabled {
            match JsonlSessionStore::create() {
                Ok(persist) => {
                    info!(
                        "session persistence enabled at {}",
                        persist.path().display()
                    );
                    builder.with_persistence(Arc::new(persist))
                }
                Err(e) => {
                    warn!("failed to initialize session persistence: {e}");
                    builder
                }
            }
        } else {
            builder
        };
        Arc::new(builder)
    };
    let router = Arc::new(ToolRouter::from_path(".").await);
    let skills = config.skills.to_skill_config();
    let tool_approval_profile = config.approval.to_tool_approval_profile();

    info!(
        tool_count = router.definitions().len(),
        "registered default tools through the extracted tools crate"
    );

    match cli_args.mode {
        CliMode::Client => {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            acp::run_interactive_cli_via_acp(
                Arc::clone(&model),
                Arc::clone(&store),
                Arc::clone(&router),
                acp::CliSessionConfig {
                    skills,
                    tool_approval_profile,
                    resume_session_id: cli_args.resume_session_id,
                },
                &mut input,
                io::stdout(),
            )
            .await?;
        }
        CliMode::Server => {
            acp::run_sdk_stdio_agent(model, store, router, skills, tool_approval_profile).await?;
        }
    }

    Ok(())
}
