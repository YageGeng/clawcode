//! In-process ACP server bootstrap for the local TUI.

use std::sync::Arc;

use agent_client_protocol::ByteStreams;
use kernel::Kernel;
use provider::factory::LlmFactory;
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tools::{FsBackend, LocalTerminalBackend, ToolRegistry};

pub mod fs;

/// ACP byte transport backed by in-memory duplex streams.
pub type InProcessTransport = ByteStreams<Compat<DuplexStream>, Compat<DuplexStream>>;

/// Running in-process ACP server task.
pub struct InProcessAcpServer {
    /// Background task serving the ACP agent side of the duplex transport.
    task: JoinHandle<()>,
}

impl InProcessAcpServer {
    /// Stops the in-process ACP server task.
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Starts the clawcode ACP agent in-process and returns the client-side transport.
pub fn start() -> anyhow::Result<(InProcessTransport, InProcessAcpServer)> {
    let config = config::load()?;
    let fs_router = Arc::new(acp::backend::fs::AcpClientFsRouter::default());
    let fs_backend: Arc<dyn FsBackend> =
        Arc::new(acp::backend::fs::AcpFsBackend::new(Arc::clone(&fs_router)));
    let tools = Arc::new(ToolRegistry::new());
    // TUI executes shell locally; only filesystem I/O is delegated to the ACP client.
    tools.register_builtins_with_backends(fs_backend, Arc::new(LocalTerminalBackend::new()));

    let kernel = Kernel::new(
        Arc::new(LlmFactory::new(config.clone())),
        config,
        Arc::clone(&tools),
    );
    kernel.register_agent_tools();

    let agent = Arc::new(acp::agent::ClawcodeAgent::with_fs_router(
        Arc::new(kernel),
        fs_router,
    ));

    // Two one-way duplex streams keep the TUI connected to the exact ACP agent
    // built in this process, avoiding stale external binaries during development.
    let (client_outgoing, agent_incoming) = tokio::io::duplex(64 * 1024);
    let (agent_outgoing, client_incoming) = tokio::io::duplex(64 * 1024);
    let client_io = ByteStreams::new(client_outgoing.compat_write(), client_incoming.compat());
    let agent_io = ByteStreams::new(agent_outgoing.compat_write(), agent_incoming.compat());

    let task = tokio::spawn(async move {
        if let Err(error) = agent.serve(agent_io).await {
            tracing::error!(%error, "in-process ACP agent failed");
        }
    });

    Ok((client_io, InProcessAcpServer { task }))
}
