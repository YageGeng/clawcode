//! Session lifecycle Hook examples.

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionRegistrar,
    SessionBeforeCompactPoint, SessionBeforeForkPoint,
    SessionBeforeSwitchPoint, SessionBeforeTreePoint, SessionCompactPoint,
    SessionInfoChangedPoint, SessionShutdownPoint, SessionStartPoint,
    SessionTreePoint,
};
use protocol::{
    SessionBeforeCompactEvent, SessionBeforeCompactResult,
    SessionBeforeForkEvent, SessionBeforeForkResult, SessionBeforeSwitchEvent,
    SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionCancelResult,
    SessionCompactEvent, SessionInfoChangedEvent, SessionShutdownEvent,
    SessionStartEvent, SessionTreeEvent,
};

struct SessionPolicy;

#[async_trait]
impl ExtensionHandler<SessionBeforeSwitchPoint> for SessionPolicy {
    /// Allows session replacement after inspecting the requested destination.
    async fn handle(
        &self,
        event: &SessionBeforeSwitchEvent,
        _context: &ExtensionContext,
    ) -> Result<SessionCancelResult, ExtensionError> {
        let _destination = &event.target_session_id;
        Ok(SessionCancelResult { cancel: false })
    }
}

#[async_trait]
impl ExtensionHandler<SessionBeforeForkPoint> for SessionPolicy {
    /// Allows forks and restores the selected conversation in the new session.
    async fn handle(
        &self,
        event: &SessionBeforeForkEvent,
        _context: &ExtensionContext,
    ) -> Result<SessionBeforeForkResult, ExtensionError> {
        let _selected_entry = &event.entry_id;
        Ok(SessionBeforeForkResult::default())
    }
}

#[async_trait]
impl ExtensionHandler<SessionBeforeCompactPoint> for SessionPolicy {
    /// Leaves compaction to the kernel while exposing where a custom summary can be returned.
    async fn handle(
        &self,
        event: &SessionBeforeCompactEvent,
        _context: &ExtensionContext,
    ) -> Result<SessionBeforeCompactResult, ExtensionError> {
        let _branch_size = event.branch_entries.len();
        Ok(SessionBeforeCompactResult::default())
    }
}

#[async_trait]
impl ExtensionHandler<SessionBeforeTreePoint> for SessionPolicy {
    /// Adds summary guidance without cancelling tree navigation.
    async fn handle(
        &self,
        event: &SessionBeforeTreeEvent,
        _context: &ExtensionContext,
    ) -> Result<SessionBeforeTreeResult, ExtensionError> {
        Ok(SessionBeforeTreeResult::builder()
            .custom_instructions(event.user_wants_summary.then(|| {
                "Preserve decisions and unresolved risks.".to_string()
            }))
            .build())
    }
}

struct SessionObserver;

#[async_trait]
impl ExtensionHandler<SessionStartPoint> for SessionObserver {
    /// Observes why a session runtime was started.
    async fn handle(
        &self,
        event: &SessionStartEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _reason = event.reason;
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<SessionInfoChangedPoint> for SessionObserver {
    /// Observes normalized session-name changes.
    async fn handle(
        &self,
        event: &SessionInfoChangedEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _name = &event.name;
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<SessionCompactPoint> for SessionObserver {
    /// Observes the persisted result of compaction.
    async fn handle(
        &self,
        event: &SessionCompactEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _compaction_entry = &event.compaction.entry_id;
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<SessionTreePoint> for SessionObserver {
    /// Observes the final tree destination and optional summary entry.
    async fn handle(
        &self,
        event: &SessionTreeEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _destination = &event.new_leaf_id;
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<SessionShutdownPoint> for SessionObserver {
    /// Observes shutdown before the session context becomes stale.
    async fn handle(
        &self,
        event: &SessionShutdownEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let _reason = event.reason;
        Ok(())
    }
}

/// Registers every session lifecycle hook.
pub(super) fn register(
    registrar: &mut ExtensionRegistrar,
) -> Result<(), ExtensionError> {
    registrar.on::<SessionStartPoint, _>(SessionObserver)?;
    registrar.on::<SessionInfoChangedPoint, _>(SessionObserver)?;
    registrar.on::<SessionBeforeSwitchPoint, _>(SessionPolicy)?;
    registrar.on::<SessionBeforeForkPoint, _>(SessionPolicy)?;
    registrar.on::<SessionBeforeCompactPoint, _>(SessionPolicy)?;
    registrar.on::<SessionCompactPoint, _>(SessionObserver)?;
    registrar.on::<SessionBeforeTreePoint, _>(SessionPolicy)?;
    registrar.on::<SessionTreePoint, _>(SessionObserver)?;
    registrar.on::<SessionShutdownPoint, _>(SessionObserver)
}
