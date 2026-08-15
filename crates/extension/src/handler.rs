use async_trait::async_trait;

use crate::{
    ExtensionCommandContext, ExtensionContext, ExtensionError, ExtensionPoint,
};

/// Strongly typed handler for one extension lifecycle point.
#[async_trait]
pub trait ExtensionHandler<P: ExtensionPoint>: Send + Sync {
    /// Handles one point-specific event and returns its point-specific output.
    async fn handle(
        &self,
        event: &P::Event,
        context: &ExtensionContext,
    ) -> Result<P::Output, ExtensionError>;
}

/// Handler for one registered non-UI extension command.
#[async_trait]
pub trait ExtensionCommandHandler: Send + Sync {
    /// Handles textual and structured command arguments with replacement-safe context.
    async fn handle(
        &self,
        arguments: &str,
        parameters: &serde_json::Value,
        context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError>;
}
