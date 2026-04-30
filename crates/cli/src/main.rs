mod config;

use std::{fs::OpenOptions, io, path::PathBuf, sync::Arc};

use clap::Parser;
use config::AppConfig;
use kernel::{model::FactoryLlmAgentModel, session::InMemorySessionStore};
use llm::providers::LlmModelFactory;
use store::JsonlSessionStore;
use tools::ToolRouter;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// ACP-powered coding assistant CLI.
#[derive(Parser)]
#[command(name = "clawcode", version)]
struct CliArgs {
    /// Run as ACP stdio server instead of interactive client.
    #[arg(long)]
    serve: bool,

    /// List persisted sessions.
    #[arg(short = 'l', long)]
    list_sessions: bool,

    /// Resume a persisted session by ID.
    #[arg(short = 'r', long, value_name = "SESSION_ID")]
    resume: Option<String>,

    /// Path to the log file (default: cli.log).
    #[arg(long, value_name = "PATH", default_value = "cli.log")]
    log: PathBuf,
}

/// Builds the CLI model adapter selected by the loaded app config.
fn build_agent_model(
    config: &AppConfig,
) -> Result<FactoryLlmAgentModel, Box<dyn std::error::Error>> {
    let factory = LlmModelFactory::try_from_config(config.llm.clone())?;
    let model = factory.completion_model_ref(config.current_model_ref())?;
    Ok(FactoryLlmAgentModel::new(model))
}

/// Initializes tracing output for the ACP CLI process.
fn init_tracing(log_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(false)
        .without_time()
        .with_writer(log_file)
        .try_init();
    Ok(())
}

fn relative_time(now: chrono::NaiveDateTime, then: chrono::NaiveDateTime) -> String {
    let delta = (now - then).num_seconds().max(0);
    match delta {
        d if d < 60 => "just now".to_string(),
        d if d < 3600 => format!("{}m ago", d / 60),
        d if d < 86400 => format!("{}h ago", d / 3600),
        d if d < 172800 => "yesterday".to_string(),
        d if d < 604800 => format!("{}d ago", d / 86400),
        _ => then.format("%m-%d").to_string(),
    }
}

fn session_preview(path: &std::path::Path) -> String {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    for _ in 0..3 {
        line.clear();
        if std::io::BufRead::read_line(&mut reader, &mut line).is_err() {
            break;
        }
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&line)
            && map.get("type").and_then(|v| v.as_str()) == Some("turn_started")
            && let Some(text) = map.get("user_text").and_then(|v| v.as_str())
        {
            let preview: String = text.chars().take(50).collect();
            if text.chars().count() > 50 {
                return format!("{preview}...");
            }
            return preview;
        }
    }
    String::new()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli_args = CliArgs::parse();
    init_tracing(&cli_args.log)?;
    let config = config::app_config();

    // Handle --list-sessions: print and exit.
    if cli_args.list_sessions {
        match store::list_sessions() {
            Ok(sessions) if sessions.is_empty() => {
                println!("No persisted sessions found.");
            }
            Ok(sessions) => {
                let now = chrono::Utc::now().naive_utc();
                println!("{:<14} {:<14} {:<6} PREVIEW", "ID", "WHEN", "TURNS");
                for s in &sessions {
                    let when = relative_time(now, s.modified_at);
                    let preview = session_preview(&s.path);
                    println!("{:<14} {:<14} {:<6} {}", s.id, when, s.turn_count, preview);
                }
            }
            Err(e) => eprintln!("Failed to list sessions: {e}"),
        }
        return Ok(());
    }

    // Resolve the existing session file path when --resume is set so new
    // turns append to the same file instead of creating a fresh one.
    let resume_path = cli_args
        .resume
        .as_ref()
        .and_then(|id| store::find_session_by_id(id));

    // When --resume is used but the session file doesn't exist, fail early.
    if let Some(ref id) = cli_args.resume
        && resume_path.is_none()
    {
        eprintln!("error: session not found: {id}");
        std::process::exit(1);
    }

    let mode_label = if cli_args.serve { "server" } else { "client" };
    info!(
        current_model = %config.current_model_ref(),
        provider_count = config.llm.providers.len(),
        mode = mode_label,
        "starting ACP cli agent"
    );

    let model = Arc::new(build_agent_model(&config)?);

    let store = {
        let builder = InMemorySessionStore::default();
        let persistence_enabled = config.persistence.enabled;
        let builder = if persistence_enabled {
            let persist_result = if let Some(ref path) = resume_path {
                JsonlSessionStore::create_at(path)
            } else {
                JsonlSessionStore::create()
            };
            match persist_result {
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

    if cli_args.serve {
        acp::run_sdk_stdio_agent(model, store, router, skills, tool_approval_profile).await?;
    } else {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        acp::run_interactive_cli_via_acp(
            Arc::clone(&model),
            Arc::clone(&store),
            Arc::clone(&router),
            acp::CliSessionConfig {
                skills,
                tool_approval_profile,
                resume_session_id: cli_args.resume,
            },
            &mut input,
            io::stdout(),
        )
        .await?;
    }

    Ok(())
}
