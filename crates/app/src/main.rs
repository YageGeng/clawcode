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
    // Capture the immutable ACP replay batching policy before the config handle
    // moves into the composition factory.
    let max_batch_size = config.current().app.recovery.max_batch_size;
    let application = ApplicationFactory::new(config).build()?;
    let transport_result: anyhow::Result<()> = match cli.command {
        Command::Stdio => {
            tracing::info!(
                "starting {} stdio transport",
                ProductIdentity::NAME
            );
            let server = AcpServerFactory::new(
                Arc::clone(&application.kernel),
                Arc::clone(&application.id_generator),
                max_batch_size,
            );
            let result = tokio::select! {
                result = server.serve_stdio() => result.map_err(anyhow::Error::from),
                () = application.kernel.wait_for_shutdown() => Ok(()),
                // Convert Ctrl-C into an orderly transport shutdown so the shared
                // terminal manager can reap every managed process below.
                signal = tokio::signal::ctrl_c() => signal.map_err(anyhow::Error::from),
            };
            tracing::info!("stopped {} stdio transport", ProductIdentity::NAME);
            result
        }
        Command::Serve {
            bind,
            path,
            web_root,
        } => async {
            let bind = LocalBindAddress::try_from(bind)?;
            let router = application.http_router(
                    WebServerOptions::builder()
                        .acp_path(path)
                        .web_root(web_root)
                        .browser_origin(bind.http_origin())
                        .health_endpoint(true)
                        .max_batch_size(max_batch_size)
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
                        tokio::select! {
                            () = shutdown_kernel.wait_for_shutdown() => {},
                            // Axum's shutdown future cannot return the signal error, so
                            // preserve it in logs while still allowing cleanup to run.
                            signal = tokio::signal::ctrl_c() => {
                                if let Err(error) = signal {
                                    tracing::warn!("failed to listen for Ctrl-C: {}", error);
                                }
                            },
                        }
                    })
                    .await?;
                tracing::info!("stopped {} HTTP transport", ProductIdentity::NAME);
                Ok(())
            }
            .await,
    };
    let shutdown_result =
        application.kernel.shutdown().await.map_err(Into::into);
    finalize_transport(transport_result, shutdown_result)
}

/// Preserves the transport failure while still reporting shutdown-only errors.
fn finalize_transport(
    transport: anyhow::Result<()>,
    shutdown: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (transport, shutdown) {
        (Ok(()), result) => result,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(shutdown_error)) => {
            tracing::warn!(
                "application shutdown also failed after transport error: {}",
                shutdown_error
            );
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::finalize_transport;

    /// Transport errors remain primary while shutdown errors surface on clean exit.
    #[test]
    fn transport_finalization_preserves_the_primary_error() {
        let transport_error = finalize_transport(
            Err(anyhow::anyhow!("transport failed")),
            Err(anyhow::anyhow!("shutdown failed")),
        )
        .expect_err("transport error");
        assert_eq!(transport_error.to_string(), "transport failed");

        let shutdown_error =
            finalize_transport(Ok(()), Err(anyhow::anyhow!("shutdown failed")))
                .expect_err("shutdown error");
        assert_eq!(shutdown_error.to_string(), "shutdown failed");
    }
}
