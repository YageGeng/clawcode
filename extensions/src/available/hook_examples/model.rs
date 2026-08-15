//! Model and reasoning-selection Hook examples.

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionRegistrar,
    ModelSelectPoint, ThinkingLevelSelectPoint,
};
use protocol::{ModelSelectEvent, ThinkingLevelSelectEvent};

struct ModelObserver;

#[async_trait]
impl ExtensionHandler<ModelSelectPoint> for ModelObserver {
    /// Observes the complete selected model profile and change source.
    async fn handle(
        &self,
        event: &ModelSelectEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _selection = (&event.model, event.source);
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<ThinkingLevelSelectPoint> for ModelObserver {
    /// Observes effective reasoning-level changes after model clamping.
    async fn handle(
        &self,
        event: &ThinkingLevelSelectEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _levels = (event.previous_level, event.level);
        Ok(())
    }
}

/// Registers model and reasoning selection hooks.
pub(super) fn register(
    registrar: &mut ExtensionRegistrar,
) -> Result<(), ExtensionError> {
    registrar.on::<ModelSelectPoint, _>(ModelObserver)?;
    registrar.on::<ThinkingLevelSelectPoint, _>(ModelObserver)
}
