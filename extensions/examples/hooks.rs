//! Compile-checks every non-UI Hook example without enabling it by default.

use extension::{ExtensionFactory, ExtensionModule};
use protocol::ExtensionId;

#[path = "../src/available/hook_examples/mod.rs"]
mod hook_examples;

/// Builds the example catalogue and creates one session-local module.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let catalog =
        extension::ExtensionCatalog::new(vec![hook_examples::definition()])?;
    let enabled = [ExtensionId::try_from(hook_examples::EXTENSION_ID)?];
    let factory = catalog.select(&enabled)?;
    let modules: Vec<std::sync::Arc<dyn ExtensionModule>> =
        factory.create_modules()?;
    let _module_count = modules.len();
    Ok(())
}
