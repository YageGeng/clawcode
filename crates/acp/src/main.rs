//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use kernel::Kernel;
use provider::factory::LlmFactory;
use tools::ToolRegistry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = config::load()?;

    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let tools = Arc::new(ToolRegistry::new());
    tools.register_builtins();

    let kernel = Arc::new(Kernel::new(llm_factory, config, tools));
    kernel.register_agent_tools();

    acp::run(kernel).await?;

    Ok(())
}
