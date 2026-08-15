use protocol::{ModelSelectEvent, ThinkingLevelSelectEvent};

use crate::{
    ExtensionContext, ExtensionRuntime, ModelSelectPoint,
    ThinkingLevelSelectPoint,
};

impl ExtensionRuntime {
    /// Notifies extensions after the active model changes.
    pub async fn emit_model_select(
        &self,
        event: &ModelSelectEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<ModelSelectPoint>(event, context).await;
    }

    /// Notifies extensions after the active thinking level changes.
    pub async fn emit_thinking_level_select(
        &self,
        event: &ThinkingLevelSelectEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<ThinkingLevelSelectPoint>(event, context)
            .await;
    }
}
