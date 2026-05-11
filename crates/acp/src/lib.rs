//! Clawcode ACP bridge.
//!
//! Implements the Agent Client Protocol (ACP) over stdio,
//! translating between the clawcode internal protocol and
//! the ACP schema types for Zed editor integration.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod agent;

use std::sync::Arc;

use agent_client_protocol::ByteStreams;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use protocol::AgentKernel;

/// Start the ACP agent over stdio transport.
///
/// # Errors
///
/// Returns an error if the ACP transport fails.
pub async fn run(kernel: Arc<dyn AgentKernel>) -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();

    let agent = Arc::new(agent::ClawcodeAgent::new(kernel));
    agent
        .serve(ByteStreams::new(stdout, stdin))
        .await
        .map_err(|e| std::io::Error::other(format!("ACP error: {e}")))
}
