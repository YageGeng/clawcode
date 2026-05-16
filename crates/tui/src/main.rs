//! Entry point for the clawcode TUI binary.

use agent_client_protocol::schema::SessionId;
use clap::Parser;

/// Command-line options for the local TUI.
#[derive(Debug, Parser)]
#[command(name = "claw-tui", version, about = "Local terminal UI for clawcode")]
struct Cli {
    /// List persisted sessions for the current working directory and exit.
    #[arg(long, conflicts_with = "resume")]
    list_sessions: bool,

    /// Resume a persisted session id instead of creating a new session.
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,

    /// Disable alternate screen mode.
    #[arg(long)]
    no_alt_screen: bool,
}

/// Run the TUI binary.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cwd = std::env::current_dir()?;
    let resume = cli.resume.map(SessionId::new);

    if cli.list_sessions {
        tui::app::list_sessions(cwd).await
    } else {
        tui::app::run(cwd, resume, !cli.no_alt_screen).await
    }
}
