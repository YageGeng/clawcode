//! Clawcode ACP bridge.
//!
//! Implements the Agent Client Protocol (ACP) over stdio,
//! translating between the clawcode internal protocol and
//! the ACP schema types for Zed editor integration.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod agent;
pub mod fs_backend;
pub mod log;

use std::sync::Arc;

use agent_client_protocol::ByteStreams;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use fs_backend::AcpClientFsRouter;
use protocol::AgentKernel;

/// Start the ACP agent over stdio transport.
///
/// # Errors
///
/// Returns an error if the ACP transport fails.
pub async fn run(kernel: Arc<dyn AgentKernel>) -> std::io::Result<()> {
    run_with_fs_router(kernel, Arc::new(AcpClientFsRouter::default())).await
}

/// Start the ACP agent over stdio transport using a filesystem router.
///
/// # Errors
///
/// Returns an error if the ACP transport fails.
pub async fn run_with_fs_router(
    kernel: Arc<dyn AgentKernel>,
    fs_router: Arc<AcpClientFsRouter>,
) -> std::io::Result<()> {
    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();

    let agent = Arc::new(agent::ClawcodeAgent::with_fs_router(kernel, fs_router));
    agent
        .serve(ByteStreams::new(stdout, stdin))
        .await
        .map_err(|e| std::io::Error::other(format!("ACP error: {e}")))
}
