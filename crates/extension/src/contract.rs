use std::sync::Arc;

use async_trait::async_trait;
use protocol::{
    ExtensionCommandDefinition, ExtensionContext, ExtensionDirective,
    ExtensionEvent, ExtensionFlagDefinition, ExtensionProviderRegistration,
};

use crate::ExtensionPipeline;

/// Typed errors surfaced while constructing or invoking extensions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtensionError {
    /// Extension registration or execution failed.
    #[error("extension failed: {0}")]
    Handler(String),
}

/// One extension participant that observes non-UI pi lifecycle events.
#[async_trait]
pub trait Extension: Send + Sync {
    /// Returns tools contributed by this extension before sessions start.
    fn tools(&self) -> Vec<Arc<dyn tools::AgentTool>> {
        Vec::new()
    }

    /// Returns non-UI commands accepted by this extension.
    fn commands(&self) -> Vec<ExtensionCommandDefinition> {
        Vec::new()
    }

    /// Returns CLI flags contributed by this extension.
    fn flags(&self) -> Vec<ExtensionFlagDefinition> {
        Vec::new()
    }

    /// Returns provider registrations for host-side factory application.
    fn providers(&self) -> Vec<ExtensionProviderRegistration> {
        Vec::new()
    }

    /// Handles one lifecycle event and returns a control directive.
    async fn handle(
        &self,
        event: &ExtensionEvent,
        context: &ExtensionContext,
    ) -> Result<ExtensionDirective, ExtensionError>;
}

/// Factory interface used by kernel construction to acquire extension pipelines.
pub trait ExtensionFactory: Send + Sync {
    /// Creates an ordered extension pipeline for one kernel lifecycle.
    fn create(&self) -> Result<ExtensionPipeline, ExtensionError>;
}
