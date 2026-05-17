//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use acp::fs_backend::{AcpClientFsRouter, AcpFsBackend};
use kernel::Kernel;
use provider::factory::LlmFactory;
use tools::{FsBackend, ToolRegistry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    acp::log::init_logging()?;

    let config = config::load()?;

    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let fs_router = Arc::new(AcpClientFsRouter::default());
    let fs_backend: Arc<dyn FsBackend> = Arc::new(AcpFsBackend::new(Arc::clone(&fs_router)));
    let tools = Arc::new(ToolRegistry::new());
    tools.register_builtins_with_fs_backend(fs_backend);

    let kernel = Arc::new(Kernel::new(llm_factory, config, tools));
    kernel.register_agent_tools();

    acp::run_with_fs_router(kernel, fs_router).await?;

    Ok(())
}
