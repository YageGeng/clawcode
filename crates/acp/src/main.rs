//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use acp::backend::fs::{AcpClientFsRouter, AcpFsBackend};
use acp::backend::terminal::{AcpClientTerminalRouter, AcpTerminalBackend};
use kernel::Kernel;
use provider::factory::LlmFactory;
use tools::builtin::fs::FsToolSet;
use tools::{FsBackend, TerminalBackend, ToolRegistry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    acp::log::init_logging()?;

    let config = config::load()?;

    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let fs_router = Arc::new(AcpClientFsRouter::default());
    let terminal_router = Arc::new(AcpClientTerminalRouter::default());
    let fs_backend: Arc<dyn FsBackend> = Arc::new(AcpFsBackend::new(Arc::clone(&fs_router)));
    let terminal_backend: Arc<dyn TerminalBackend> =
        Arc::new(AcpTerminalBackend::new(Arc::clone(&terminal_router)));
    let tools = Arc::new(ToolRegistry::new());
    tools.register_builtins_with_backends(fs_backend, terminal_backend, FsToolSet::Hashline);

    let kernel = Arc::new(Kernel::new(llm_factory, config, tools));
    kernel.register_agent_tools();

    acp::run_with_routers(kernel, fs_router, terminal_router).await?;

    Ok(())
}
