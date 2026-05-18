//! Clawcode ACP bridge.
//!
//! Implements the Agent Client Protocol (ACP) over stdio,
//! translating between the clawcode internal protocol and
//! the ACP schema types for Zed editor integration.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod agent;
pub mod backend;
pub mod log;

use std::sync::Arc;

use agent_client_protocol::ByteStreams;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use backend::fs::AcpClientFsRouter;
use backend::terminal::AcpClientTerminalRouter;
use protocol::AgentKernel;

/// Start the ACP agent over stdio transport with default routers.
///
/// # Errors
///
/// Returns an error if the ACP transport fails.
pub async fn run(kernel: Arc<dyn AgentKernel>) -> std::io::Result<()> {
    run_with_routers(
        kernel,
        Arc::new(AcpClientFsRouter::default()),
        Arc::new(AcpClientTerminalRouter::default()),
    )
    .await
}

/// Start the ACP agent over stdio transport using a filesystem router
/// and a default terminal router.
///
/// # Errors
///
/// Returns an error if the ACP transport fails.
pub async fn run_with_fs_router(
    kernel: Arc<dyn AgentKernel>,
    fs_router: Arc<AcpClientFsRouter>,
) -> std::io::Result<()> {
    run_with_routers(
        kernel,
        fs_router,
        Arc::new(AcpClientTerminalRouter::default()),
    )
    .await
}

/// Start the ACP agent over stdio transport using custom routers.
///
/// # Errors
///
/// Returns an error if the ACP transport fails.
pub async fn run_with_routers(
    kernel: Arc<dyn AgentKernel>,
    fs_router: Arc<AcpClientFsRouter>,
    terminal_router: Arc<AcpClientTerminalRouter>,
) -> std::io::Result<()> {
    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();

    let agent = Arc::new(agent::ClawcodeAgent::with_routers(
        kernel,
        fs_router,
        terminal_router,
    ));
    agent
        .serve(ByteStreams::new(stdout, stdin))
        .await
        .map_err(|e| std::io::Error::other(format!("ACP error: {e}")))
}
