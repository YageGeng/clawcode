use std::net::SocketAddr;
use std::sync::Arc;

use acp::AcpServerFactory;
use app::{
    ApplicationFactory, LocalBindAddress, LoggingFactory, WebServerOptions,
};
use clap::{Parser, Subcommand};
use protocol::ProductIdentity;

/// Command-line entry point for ACP stdio and browser transports.
#[derive(Debug, Parser)]
#[command(name = ProductIdentity::SLUG, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Supported transport modes for the headless agent service.
#[derive(Debug, Subcommand)]
enum Command {
    /// Serve one ACP connection over stdin and stdout.
    Stdio,
    /// Serve ACP HTTP/SSE and WebSocket on the same endpoint.
    Serve {
        /// TCP address used by the HTTP server.
        #[arg(long, default_value = "127.0.0.1:3000")]
        bind: SocketAddr,
        /// ACP endpoint path.
        #[arg(long, default_value = ProductIdentity::DEFAULT_ACP_PATH)]
        path: String,
        /// Vite production output directory served by the application.
        #[arg(long, default_value = "web/dist")]
        web_root: std::path::PathBuf,
    },
}

/// Loads application dependencies and runs the selected ACP transport.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logging is immutable process configuration, so load TOML before
    // installing the one global subscriber used by every transport.
    let config = config::load()?;
    LoggingFactory::from_config(&config.current().logging)?.install()?;
    let result = run(config).await;
    if let Err(error) = &result {
        tracing::error!("application terminated with error: {}", error);
    }
    result
}

/// Parses CLI options, composes dependencies, and serves the selected transport.
async fn run(config: config::ConfigHandle) -> anyhow::Result<()> {
    let cli = Cli::parse();
    let application = ApplicationFactory::new(config).build()?;
    match cli.command {
        Command::Stdio => {
            tracing::info!(
                "starting {} stdio transport",
                ProductIdentity::NAME
            );
            let server = AcpServerFactory::new(
                Arc::clone(&application.kernel),
                Arc::clone(&application.id_generator),
            );
            tokio::select! {
                result = server.serve_stdio() => result?,
                () = application.kernel.wait_for_shutdown() => {},
            }
            tracing::info!("stopped {} stdio transport", ProductIdentity::NAME);
        }
        Command::Serve {
            bind,
            path,
            web_root,
        } => {
            let bind = LocalBindAddress::try_from(bind)?;
            let router = application.http_router(
                WebServerOptions::builder()
                    .acp_path(path)
                    .web_root(web_root)
                    .browser_origin(bind.http_origin())
                    .health_endpoint(true)
                    .build(),
            )?;
            let bind = bind.into_inner();
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(
                "starting {} HTTP transport at http://{}",
                ProductIdentity::NAME,
                bind
            );
            let shutdown_kernel = Arc::clone(&application.kernel);
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    shutdown_kernel.wait_for_shutdown().await;
                })
                .await?;
            tracing::info!("stopped {} HTTP transport", ProductIdentity::NAME);
        }
    }
    Ok(())
}
