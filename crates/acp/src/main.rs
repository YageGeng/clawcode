//! Entry point for the clawcode ACP binary.

use std::sync::Arc;

use config::{AppConfig, ConfigHandle};
use kernel::Kernel;
use provider::factory::LlmFactory;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = ConfigHandle::from_config(AppConfig::default());
    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let kernel = Arc::new(Kernel::new(llm_factory.clone(), config));

    acp::run(kernel, llm_factory).await
}
