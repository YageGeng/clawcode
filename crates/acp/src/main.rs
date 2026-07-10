//! Entry point for the clawcode ACP binary.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use acp::backend::fs::{AcpClientFsRouter, AcpFsBackend};
use acp::backend::terminal::{AcpClientTerminalRouter, AcpTerminalBackend};
use clap::Parser;
use kernel::Kernel;
use provider::factory::LlmFactory;
use tools::builtin::{BuiltinToolConfig, fs::FsToolSet};
use tools::{FsBackend, TerminalBackend, ToolRegistry};

/// Command-line options for the ACP binary.
#[derive(Debug, Parser)]
#[command(name = "claw-acp", version, about = "ACP bridge for clawcode")]
struct Cli {
    /// Serve the ACP agent over HTTP/WebSocket with the built-in web UI.
    #[arg(long)]
    http: bool,

    /// Host interface used by HTTP mode.
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,

    /// Port used by HTTP mode; 0 asks the OS to assign a free port.
    #[arg(long, default_value_t = 0)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    acp::log::init_logging()?;

    let config = config::load()?;

    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let fs_router = Arc::new(AcpClientFsRouter::default());
    let terminal_router = Arc::new(AcpClientTerminalRouter::default());
    let fs_backend: Arc<dyn FsBackend> =
        Arc::new(AcpFsBackend::new(Arc::clone(&fs_router)));
    let terminal_backend: Arc<dyn TerminalBackend> =
        Arc::new(AcpTerminalBackend::new(Arc::clone(&terminal_router)));
    let tools = Arc::new(ToolRegistry::new());
    let tool_config = config.current().tools;
    tools.register_builtins_with_backends_and_config(
        fs_backend,
        terminal_backend,
        FsToolSet::Hashline,
        BuiltinToolConfig {
            enable_fs: tool_config.enable_fs,
            enable_shell: tool_config.enable_shell,
        },
    );

    let kernel = Arc::new(Kernel::new(llm_factory, config, tools));
    kernel.register_agent_tools();

    if cli.http {
        let options = acp::http::HttpServerOptions {
            bind: SocketAddr::new(cli.host, cli.port),
            ..Default::default()
        };
        acp::http::run_with_routers(
            kernel,
            fs_router,
            terminal_router,
            options,
        )
        .await?;
    } else {
        acp::run_with_routers(kernel, fs_router, terminal_router).await?;
    }

    Ok(())
}
