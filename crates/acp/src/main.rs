//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use kernel::Kernel;
use kernel::tool::ToolRegistry;
use provider::factory::LlmFactory;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = config::load()?;

    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let tools = Arc::new(ToolRegistry::new());
    let kernel = Arc::new(Kernel::new(llm_factory, config, tools));

    acp::run(kernel).await?;

    Ok(())
}
