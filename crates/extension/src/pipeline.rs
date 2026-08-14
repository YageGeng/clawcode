use std::sync::Arc;

use protocol::{
    ExtensionCommandDefinition, ExtensionContext, ExtensionEffects,
    ExtensionEvent, ExtensionFlagDefinition, ExtensionProviderRegistration,
};

use crate::{Extension, ExtensionError, ExtensionFactory};

/// Ordered non-UI lifecycle dispatcher.
pub struct ExtensionPipeline {
    extensions: Vec<Arc<dyn Extension>>,
}

impl ExtensionPipeline {
    /// Creates a pipeline preserving the provided registration order.
    #[must_use]
    pub fn new(extensions: Vec<Arc<dyn Extension>>) -> Self {
        Self { extensions }
    }

    /// Dispatches one event while composing every non-blocking effect.
    pub async fn dispatch(
        &self,
        event: &ExtensionEvent,
        context: &ExtensionContext,
    ) -> Result<ExtensionEffects, ExtensionError> {
        let mut effects = ExtensionEffects::default();
        for extension in &self.extensions {
            let directive = extension.handle(event, context).await?;
            if !effects.apply(directive) {
                break;
            }
        }
        Ok(effects)
    }

    /// Returns all extension tools in deterministic registration order.
    pub fn tools(&self) -> Vec<Arc<dyn tools::AgentTool>> {
        self.extensions
            .iter()
            .flat_map(|extension| extension.tools())
            .collect()
    }

    /// Returns all non-UI command definitions in deterministic order.
    pub fn commands(&self) -> Vec<ExtensionCommandDefinition> {
        self.extensions
            .iter()
            .flat_map(|extension| extension.commands())
            .collect()
    }

    /// Returns all CLI flag definitions in deterministic order.
    pub fn flags(&self) -> Vec<ExtensionFlagDefinition> {
        self.extensions
            .iter()
            .flat_map(|extension| extension.flags())
            .collect()
    }

    /// Returns all provider registrations for the host composition layer.
    pub fn providers(&self) -> Vec<ExtensionProviderRegistration> {
        self.extensions
            .iter()
            .flat_map(|extension| extension.providers())
            .collect()
    }
}

/// Factory backed by an already constructed ordered extension list.
pub struct StaticExtensionFactory {
    extensions: Vec<Arc<dyn Extension>>,
}

impl StaticExtensionFactory {
    /// Creates a static factory from ordered extension instances.
    #[must_use]
    pub fn new(extensions: Vec<Arc<dyn Extension>>) -> Self {
        Self { extensions }
    }
}

impl ExtensionFactory for StaticExtensionFactory {
    /// Clones extension handles into a fresh ordered pipeline.
    fn create(&self) -> Result<ExtensionPipeline, ExtensionError> {
        Ok(ExtensionPipeline::new(self.extensions.clone()))
    }
}
