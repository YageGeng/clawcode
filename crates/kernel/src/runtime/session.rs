//! Session state and lifecycle: registry, assembly, history, settings, and naming.

mod assembly;
mod close;
mod fork;
pub(super) mod model_state;
mod open;
mod ownership;
mod prompt;
pub(super) mod rename;
mod replay;
pub(super) mod resources;
pub(super) mod settings;
pub(super) mod transcript;
mod tree;

pub use tree::TreeNavigationRequest;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use protocol::{
    AgentMessage, EntryId, IdKind, RecordId, RunId, SessionId, SessionSummary,
    SessionTreeSnapshot,
};
use store::{NewRecord, RecordKind};

use crate::runtime::{Kernel, KernelError, Session};

/// Releases one exclusively reserved Session identifier when its operation exits.
pub(super) struct SessionIdReservation {
    session_id: SessionId,
    pending_sessions:
        Arc<Mutex<HashMap<SessionId, tokio::sync::watch::Sender<()>>>>,
}

impl Drop for SessionIdReservation {
    /// Removes only the Session identifier owned by this exclusive operation.
    fn drop(&mut self) {
        if let Ok(mut pending_sessions) = self.pending_sessions.lock() {
            pending_sessions.remove(&self.session_id);
        }
    }
}

impl Kernel {
    /// Lists persisted sessions by scanning v4 headers instead of a global index.
    pub fn list_sessions(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(self
            .store_factory
            .list(cwd)?
            .into_iter()
            .map(|metadata| {
                SessionSummary::builder()
                    .session_id(metadata.id)
                    .cwd(metadata.cwd)
                    .created_at_ms(metadata.created_at_ms)
                    .modified_at_ms(metadata.modified_at_ms)
                    .parent_session_id(metadata.parent_session_id)
                    .ownership(metadata.ownership)
                    .name(metadata.name)
                    .build()
            })
            .collect())
    }

    /// Returns the live Session's current display title, if it has one.
    pub fn session_title(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<String>, KernelError> {
        self.session(session_id)?.transcript.name()
    }

    /// Reads active Run state without waiting for the Session operation gate.
    pub fn session_runtime(
        &self,
        session_id: &SessionId,
    ) -> Result<protocol::SessionRuntimeSnapshot, KernelError> {
        let running = match self.session(session_id) {
            Ok(session) => session.execution.active_run()?.is_some(),
            Err(KernelError::SessionNotFound(_)) => {
                // Persisted Sessions are idle before their first Resume in this
                // process, so runtime discovery must not require loading them.
                if self.store_factory.exists(session_id).map_err(|error| {
                    tracing::warn!(
                        "failed persisted Session lookup {}: {}",
                        session_id,
                        error
                    );
                    error
                })? {
                    false
                } else {
                    return Err(KernelError::SessionNotFound(
                        session_id.clone(),
                    ));
                }
            }
            Err(error) => return Err(error),
        };
        Ok(protocol::SessionRuntimeSnapshot {
            session_id: session_id.clone(),
            running,
        })
    }

    /// Cancels the active run for a session; the next run receives a fresh token.
    pub fn cancel_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        session.execution.cancel()?;
        self.withdraw_pending(session_id)
    }

    /// Returns the compaction-aware active model context for runtime inspection.
    pub fn session_messages(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        self.session(session_id)?.transcript.history()
    }

    /// Returns all persisted entries and the active main-lane cursor.
    pub fn session_tree(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionTreeSnapshot, KernelError> {
        self.session(session_id)?.transcript.tree_snapshot()
    }

    /// Waits until a pending build or deletion reservation of `session_id` ends.
    ///
    /// Returns `false` without waiting when none is pending, so callers fall
    /// back to the Session's lifecycle instead of retrying in a tight loop.
    pub(super) async fn wait_for_session_reservation(
        &self,
        session_id: &SessionId,
    ) -> Result<bool, KernelError> {
        let pending = self
            .pending_sessions
            .lock()?
            .get(session_id)
            .map(tokio::sync::watch::Sender::subscribe);
        let Some(mut pending) = pending else {
            return Ok(false);
        };
        tracing::info!(
            "waiting for the pending build or deletion of Session {}",
            session_id
        );
        // The reservation drops its sender on both success and rollback.
        let _ = pending.changed().await;
        Ok(true)
    }

    /// Registers a constructed runtime or releases its unregistered resources on failure.
    pub(super) async fn register_session(
        &self,
        session_id: SessionId,
        runtime: &Arc<Session>,
    ) -> Result<(), KernelError> {
        let registration = (|| {
            let mut sessions = self.sessions.write()?;
            if sessions.contains_key(&session_id) {
                return Err(KernelError::DuplicateSession(session_id.clone()));
            }
            sessions.insert(session_id.clone(), Arc::clone(runtime));
            Ok(())
        })();
        if let Err(error) = registration {
            if let Err(cleanup_error) =
                Self::release_unregistered_session(runtime).await
            {
                tracing::warn!(
                    "failed to release unregistered Session {} after registration error: {}",
                    session_id,
                    cleanup_error
                );
            }
            return Err(error);
        }
        Ok(())
    }

    /// Reserves one identifier across Store opening, runtime construction, and startup hooks.
    pub(super) fn reserve_session_build(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionIdReservation, KernelError> {
        // Hold the map read lock until the pending id is inserted so registration
        // and reservation cannot both observe the identifier as absent.
        let sessions = self.sessions.read()?;
        if sessions.contains_key(session_id) {
            return Err(KernelError::DuplicateSession(session_id.clone()));
        }
        let mut pending_sessions = self.pending_sessions.lock()?;
        if pending_sessions.contains_key(session_id) {
            return Err(KernelError::DuplicateSession(session_id.clone()));
        }
        pending_sessions
            .insert(session_id.clone(), tokio::sync::watch::channel(()).0);
        Ok(SessionIdReservation {
            session_id: session_id.clone(),
            pending_sessions: Arc::clone(&self.pending_sessions),
        })
    }

    /// Reserves one identifier across runtime shutdown and persisted Store deletion.
    fn reserve_session_deletion(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionIdReservation, KernelError> {
        // Match construction's lock order and keep the map stable until the id
        // is reserved, closing both absent-runtime races around registration.
        let _sessions = self.sessions.read()?;
        let mut pending_sessions = self.pending_sessions.lock()?;
        if pending_sessions.contains_key(session_id) {
            return Err(KernelError::DuplicateSession(session_id.clone()));
        }
        pending_sessions
            .insert(session_id.clone(), tokio::sync::watch::channel(()).0);
        Ok(SessionIdReservation {
            session_id: session_id.clone(),
            pending_sessions: Arc::clone(&self.pending_sessions),
        })
    }

    /// Invalidates and shuts down a runtime that never entered the Session map.
    pub(super) async fn release_unregistered_session(
        runtime: &Arc<Session>,
    ) -> Result<(), KernelError> {
        runtime.extensions.runtime_ref().invalidate();
        runtime.mcp.shutdown().await
    }

    /// Keeps the first lifecycle error while logging a later cleanup failure.
    fn preserve_primary_error(
        primary: Result<(), KernelError>,
        secondary: Result<(), KernelError>,
        secondary_action: &str,
    ) -> Result<(), KernelError> {
        match (primary, secondary) {
            (Ok(()), result) => result,
            (Err(primary_error), Ok(())) => Err(primary_error),
            (Err(primary_error), Err(secondary_error)) => {
                tracing::warn!(
                    "failed to {} after an earlier lifecycle error: {}",
                    secondary_action,
                    secondary_error
                );
                Err(primary_error)
            }
        }
    }

    /// Removes a partially started runtime and optionally its new durable log.
    pub(super) async fn rollback_session_registration(
        &self,
        session_id: &SessionId,
        delete_persisted: bool,
    ) -> Result<(), KernelError> {
        let runtime = self.sessions.write()?.remove(session_id);
        if let Some(runtime) = runtime {
            runtime.extensions.runtime_ref().invalidate();
            let mcp_result = runtime.mcp.shutdown().await;
            let delete_result = if delete_persisted {
                self.store_factory
                    .delete(session_id)
                    .map_err(KernelError::from)
            } else {
                Ok(())
            };
            return Self::preserve_primary_error(
                mcp_result,
                delete_result,
                "delete persisted Session during registration rollback",
            );
        }
        if delete_persisted {
            self.store_factory.delete(session_id)?;
        }
        Ok(())
    }

    /// Resolves one registered session without holding the map lock afterward.
    pub(super) fn session(
        &self,
        session_id: &SessionId,
    ) -> Result<Arc<Session>, KernelError> {
        let session = self
            .sessions
            .read()?
            .get(session_id)
            .map(Arc::clone)
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        // Registered startup entries exist for Extension context construction,
        // but ordinary kernel callers may use only fully initialized Sessions.
        session.ensure_active()?;
        Ok(session)
    }

    /// Persists one message and then exposes it to subsequent model contexts.
    pub(super) fn persist_message(
        &self,
        session: &Session,
        message: &AgentMessage,
    ) -> Result<(), KernelError> {
        let entry_id = EntryId::try_from(self.id_generator.next(IdKind::Entry))
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.persist_message_at(session, message, entry_id)
    }

    /// Persists one message at an identifier reserved by queue acceptance.
    pub(super) fn persist_message_at(
        &self,
        session: &Session,
        message: &AgentMessage,
        entry_id: EntryId,
    ) -> Result<(), KernelError> {
        session.transcript.append_context_message(entry_id, message)
    }

    /// Appends one durable pi v4 lane record for run recovery and inspection.
    pub(super) fn record_operation(
        &self,
        session: &Session,
        run_id: &RunId,
        kind: RecordKind,
        payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        let record_id =
            RecordId::try_from(self.id_generator.next(IdKind::Record))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        session.transcript.append_record(
            NewRecord::builder()
                .id(record_id)
                .lane(session.transcript.lane())
                .run_id(Some(run_id.clone()))
                .kind(kind)
                .payload(payload)
                .build(),
        )?;
        Ok(())
    }
}
