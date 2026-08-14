use std::net::SocketAddr;
use std::sync::Arc;

use acp::AcpServerFactory;
use app::{ApplicationFactory, LocalBindAddress, WebServerOptions};
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
    let cli = Cli::parse();
    let application = ApplicationFactory::load()?.build()?;
    match cli.command {
        Command::Stdio => {
            AcpServerFactory::new(Arc::clone(&application.kernel))
                .serve_stdio()
                .await?;
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
            axum::serve(listener, router).await?;
        }
    }
    Ok(())
}
