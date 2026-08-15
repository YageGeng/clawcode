use std::sync::Arc;

use protocol::{ExtensionDescriptor, StaticExtensionRegistration};

use crate::{ExtensionError, ExtensionRegistrar};

/// One in-process Rust extension module created independently per session.
pub trait ExtensionModule: Send + Sync {
    /// Returns stable identity and display metadata for this module.
    fn descriptor(&self) -> ExtensionDescriptor;

    /// Registers typed handlers and per-session capabilities synchronously.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError>;
}

/// Reusable closure that creates a fresh module for one session.
pub type ExtensionModuleFactory = Arc<
    dyn Fn() -> Result<Arc<dyn ExtensionModule>, ExtensionError> + Send + Sync,
>;

/// Factory interface used before kernel and session construction.
pub trait ExtensionFactory: Send + Sync {
    /// Returns immutable declarations needed before model construction.
    fn static_registration(
        &self,
    ) -> Result<StaticExtensionRegistration, ExtensionError>;

    /// Creates new ordered module objects for one session runtime.
    fn create_modules(
        &self,
    ) -> Result<Vec<Arc<dyn ExtensionModule>>, ExtensionError>;
}

/// Factory backed by immutable declarations and repeatable module closures.
pub struct StaticExtensionFactory {
    registration: StaticExtensionRegistration,
    modules: Vec<ExtensionModuleFactory>,
}

impl StaticExtensionFactory {
    /// Creates a static factory that invokes every module closure per session.
    #[must_use]
    pub fn new(
        registration: StaticExtensionRegistration,
        modules: Vec<ExtensionModuleFactory>,
    ) -> Self {
        Self {
            registration,
            modules,
        }
    }
}

impl Default for StaticExtensionFactory {
    /// Creates a factory with no extensions or static declarations.
    fn default() -> Self {
        Self::new(StaticExtensionRegistration::default(), Vec::new())
    }
}

impl ExtensionFactory for StaticExtensionFactory {
    /// Clones immutable build-time declarations.
    fn static_registration(
        &self,
    ) -> Result<StaticExtensionRegistration, ExtensionError> {
        Ok(self.registration.clone())
    }

    /// Invokes every closure so stateful modules are never shared by sessions.
    fn create_modules(
        &self,
    ) -> Result<Vec<Arc<dyn ExtensionModule>>, ExtensionError> {
        self.modules.iter().map(|factory| factory()).collect()
    }
}
