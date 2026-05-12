//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use kernel::Kernel;
use provider::factory::LlmFactory;
use tools::ToolRegistry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = config::load()?;

    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let mut tools = ToolRegistry::new();
    tools.register_builtins();
    let tools = Arc::new(tools);
    let kernel = Arc::new(Kernel::new(llm_factory, config, tools));

    acp::run(kernel).await?;

    Ok(())
}
