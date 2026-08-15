//! Compile-checked examples for every supported non-UI extension domain.

use std::sync::Arc;

use extension::{
    CompiledExtension, ExtensionError, ExtensionModule, ExtensionRegistrar,
};
use protocol::{ExtensionDescriptor, ExtensionId, StaticExtensionRegistration};

mod agent;
mod model;
mod provider;
mod session;
mod startup;
mod tool;

/// Stable identifier used to select the complete Hook example module.
pub const EXTENSION_ID: &str = "hook-examples";

/// Example module that installs every non-policy Pi lifecycle example.
struct HookExamples;

impl HookExamples {
    /// Builds metadata shared by static and per-session declarations.
    fn extension_descriptor() -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from(EXTENSION_ID)
                .expect("hook example extension id is valid"),
            name: "Non-UI hook examples".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

impl ExtensionModule for HookExamples {
    /// Describes the compile-checked example extension.
    fn descriptor(&self) -> ExtensionDescriptor {
        Self::extension_descriptor()
    }

    /// Registers each lifecycle domain through its focused module.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        startup::register(registrar)?;
        session::register(registrar)?;
        agent::register(registrar)?;
        provider::register(registrar)?;
        model::register(registrar)?;
        tool::register(registrar)
    }
}

/// Describes the module and creates a fresh example instance for each session.
pub(crate) fn definition() -> CompiledExtension {
    let descriptor = HookExamples::extension_descriptor();
    CompiledExtension::new(
        descriptor.clone(),
        StaticExtensionRegistration::builder()
            .extensions(vec![descriptor])
            .build(),
        Arc::new(|| Ok(Arc::new(HookExamples) as Arc<dyn ExtensionModule>)),
    )
}
