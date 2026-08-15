use protocol::{
    SessionBeforeCompactEvent, SessionBeforeCompactResult,
    SessionBeforeForkEvent, SessionBeforeForkResult, SessionBeforeSwitchEvent,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionCancelResult,
    SessionCompactEvent, SessionInfoChangedEvent, SessionShutdownEvent,
    SessionStartEvent, SessionTreeEvent,
};

use crate::{
    ExtensionContext, ExtensionRuntime, SessionBeforeCompactPoint,
    SessionBeforeForkPoint, SessionBeforeSwitchPoint, SessionBeforeTreePoint,
    SessionCompactPoint, SessionInfoChangedPoint, SessionShutdownPoint,
    SessionStartPoint, SessionTreePoint,
};

impl ExtensionRuntime {
    /// Stops a pending session switch at the first cancellation decision.
    pub async fn emit_session_before_switch(
        &self,
        event: &SessionBeforeSwitchEvent,
        context: &ExtensionContext,
    ) -> SessionCancelResult {
        for registered in self.handlers::<SessionBeforeSwitchPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(event, &handler_context).await {
                Ok(result) if result.cancel => return result,
                Ok(_continue) => {}
                Err(error) => {
                    self.report::<SessionBeforeSwitchPoint>(registered, &error)
                        .await;
                }
            }
        }
        SessionCancelResult::default()
    }

    /// Stops a pending fork at the first cancellation and composes restore policy.
    pub async fn emit_session_before_fork(
        &self,
        event: &SessionBeforeForkEvent,
        context: &ExtensionContext,
    ) -> SessionBeforeForkResult {
        let mut result = SessionBeforeForkResult::default();
        for registered in self.handlers::<SessionBeforeForkPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(event, &handler_context).await {
                Ok(current) => {
                    result.skip_conversation_restore |=
                        current.skip_conversation_restore;
                    if current.cancel {
                        result.cancel = true;
                        return result;
                    }
                }
                Err(error) => {
                    self.report::<SessionBeforeForkPoint>(registered, &error)
                        .await;
                }
            }
        }
        result
    }

    /// Returns the first cancellation or complete custom compaction decision.
    pub async fn emit_session_before_compact(
        &self,
        event: &SessionBeforeCompactEvent,
        context: &ExtensionContext,
    ) -> SessionBeforeCompactResult {
        for registered in self.handlers::<SessionBeforeCompactPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(event, &handler_context).await {
                Ok(result) if result.cancel || result.compaction.is_some() => {
                    return result;
                }
                Ok(_continue) => {}
                Err(error) => {
                    self.report::<SessionBeforeCompactPoint>(
                        registered, &error,
                    )
                    .await;
                }
            }
        }
        SessionBeforeCompactResult::default()
    }

    /// Chains tree-summary overrides and stops at the first cancellation.
    pub async fn emit_session_before_tree(
        &self,
        event: &SessionBeforeTreeEvent,
        context: &ExtensionContext,
    ) -> SessionBeforeTreeResult {
        let mut composed = SessionBeforeTreeResult::default();
        for registered in self.handlers::<SessionBeforeTreePoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(event, &handler_context).await {
                Ok(result) => {
                    if result.summary.is_some() {
                        composed.summary = result.summary;
                    }
                    if result.custom_instructions.is_some() {
                        composed.custom_instructions =
                            result.custom_instructions;
                    }
                    if result.replace_instructions.is_some() {
                        composed.replace_instructions =
                            result.replace_instructions;
                    }
                    if result.label.is_some() {
                        composed.label = result.label;
                    }
                    if result.cancel {
                        composed.cancel = true;
                        return composed;
                    }
                }
                Err(error) => {
                    self.report::<SessionBeforeTreePoint>(registered, &error)
                        .await;
                }
            }
        }
        composed
    }

    /// Notifies extensions that a session runtime started.
    pub async fn emit_session_start(
        &self,
        event: &SessionStartEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<SessionStartPoint>(event, context).await;
    }

    /// Notifies extensions that session metadata changed.
    pub async fn emit_session_info_changed(
        &self,
        event: &SessionInfoChangedEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<SessionInfoChangedPoint>(event, context)
            .await;
    }

    /// Notifies extensions that context compaction completed.
    pub async fn emit_session_compact(
        &self,
        event: &SessionCompactEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<SessionCompactPoint>(event, context).await;
    }

    /// Notifies extensions that session tree navigation completed.
    pub async fn emit_session_tree(
        &self,
        event: &SessionTreeEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<SessionTreePoint>(event, context).await;
    }

    /// Notifies extensions before a session runtime is invalidated.
    pub async fn emit_session_shutdown(
        &self,
        event: &SessionShutdownEvent,
        context: &ExtensionContext,
    ) {
        self.observe::<SessionShutdownPoint>(event, context).await;
    }
}
