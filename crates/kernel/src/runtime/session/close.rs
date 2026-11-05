//! Closing and deleting Sessions after their active work has settled.

use std::sync::Arc;
use std::time::Duration;

use protocol::SessionId;

use crate::runtime::{Kernel, KernelError};

impl Kernel {
    /// Cancels active work, waits for it to settle, and releases in-memory session resources.
    pub async fn close_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        tracing::info!("started Session close {}", session_id);
        let result: Result<(), KernelError> = async {
            let session = self.session(session_id)?;
            session.background.shutdown.cancel();
            tracing::info!("abandoned subagent reports for closing session {}", session_id);
            let _run_guard = session.execution.acquire_close().await?;
            if let Err(error) = self.withdraw_pending(session_id) {
                tracing::warn!("failed queue withdrawal while closing session {}: {}", session_id, error);
            }
            let _creation_guard = session.owned_creation.write().await;
            // Transition before invoking hooks so reentrant host operations fail
            // immediately instead of waiting on the gate held by this shutdown.
            // Held to the end of shutdown, after the runtime leaves the map, so
            // waiters retry only once the id is free or the Session is active again.
            let _closing = session.begin_closing()?;
            tracing::debug!("waiting for {} queue publications while closing session {}", session.queue_publishes.len(), session_id);
            session.queue_publishes.close();
            session.queue_publishes.wait().await;
            let mut children_result = Ok(());
            for child in self.live_children(session_id)? {
                tracing::info!("started cascading close of session {} owned by {}", child, session_id);
                let result = match Box::pin(self.close_session(&child)).await {
                    Err(KernelError::SessionNotFound(_)) => Ok(()),
                    result => result,
                };
                children_result = Self::preserve_primary_error(children_result, result, "close owned child");
            }
            children_result = Self::preserve_primary_error(children_result, session.background.join().await, "join owned reporters");
            let context = match self.extension_context(&session, None, None) {
                Ok(context) => context,
                Err(error) => {
                    if let Err(abort_error) = session.abort_closing() {
                        tracing::warn!(
                            "failed to restore Session {} after shutdown setup error: {}",
                            session_id,
                            abort_error
                        );
                    }
                    return Err(error);
                }
            };
            session
                .extensions
                .runtime_ref()
                .emit_session_shutdown(
                    &protocol::SessionShutdownEvent {
                        reason: protocol::SessionShutdownReason::Quit,
                        target_session_id: None,
                    },
                    &context,
                )
                .await;
            // Once MCP teardown starts the runtime cannot safely become active
            // again, so every finalization step runs even when an earlier one fails.
            let mcp_result = Self::preserve_primary_error(children_result, session.mcp.shutdown().await, "shut down owner MCP");
            let sync_result = session.sync_store();
            let lifecycle_result = session.finish_closing();
            session.extensions.runtime_ref().invalidate();
            let removal_result = self
                .sessions
                .write()
                .map_err(KernelError::from)
                .map(|mut sessions| {
                    sessions.remove(session_id);
                });
            Self::preserve_primary_error(
                Self::preserve_primary_error(
                    Self::preserve_primary_error(
                        mcp_result,
                        sync_result,
                        "sync Store during Session close",
                    ),
                    lifecycle_result,
                    "finish Session lifecycle during close",
                ),
                removal_result,
                "remove Session runtime after close",
            )
        }
        .await;
        match result {
            Ok(()) => {
                tracing::info!("completed Session close {}", session_id);
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    "failed Session close {}: {}",
                    session_id,
                    error
                );
                Err(error)
            }
        }
    }

    /// Releases an active runtime before removing its persisted session history idempotently.
    pub async fn delete_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        tracing::info!("started Session deletion {}", session_id);
        let result: Result<(), KernelError> = async {
            // Reserve before closing so construction cannot claim the identifier
            // between runtime removal and persisted Store deletion.
            let _reservation = self.reserve_session_deletion(session_id)?;
            let cwd = match self.session(session_id) {
                Ok(session) => Some(session.cwd.clone()),
                Err(KernelError::SessionNotFound(_)) => self.store_factory.header(session_id)?.map(|header| header.cwd),
                Err(error) => return Err(error),
            };
            match self.close_session(session_id).await {
                Ok(()) | Err(KernelError::SessionNotFound(_)) => {}
                Err(error) => return Err(error),
            }
            if let Some(cwd) = cwd {
                for child in self.store_factory.headers(Some(&cwd))?.into_iter().filter(|item| item.ownership.as_ref().is_some_and(|edge| edge.owner() == session_id)) {
                    tracing::info!("started cascading deletion of session {} owned by {}", child.id, session_id);
                    Box::pin(self.delete_session(&child.id)).await?;
                    tracing::info!("completed cascading deletion of session {} owned by {}", child.id, session_id);
                }
            }
            self.store_factory.delete(session_id)?;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                tracing::info!("completed Session deletion {}", session_id);
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    "failed Session deletion {}: {}",
                    session_id,
                    error
                );
                Err(error)
            }
        }
    }

    /// Waits until a closing Session's shutdown finishes or aborts.
    ///
    /// Returns `false` without waiting when no shutdown is in progress, so a
    /// caller reports the lifecycle error instead of retrying forever.
    pub(in crate::runtime) async fn wait_for_session_close(
        &self,
        session_id: &SessionId,
    ) -> Result<bool, KernelError> {
        // A hung shutdown hook or MCP teardown must not hang callers with it;
        // past this bound they get SessionClosing and may retry.
        const CLOSE_WAIT: Duration = Duration::from_secs(10);
        let session = self.sessions.read()?.get(session_id).map(Arc::clone);
        // Shutdown already removed the runtime, which frees the id.
        let Some(session) = session else {
            return Ok(true);
        };
        let mut closing = session.execution.closing();
        if !*closing.borrow_and_update() {
            // No shutdown is running: an aborted close left the Session active,
            // a finished one is about to free the id, and a runtime reopened
            // in its place may still be starting, so retry; only a runtime
            // stuck closing with no shutdown is reported.
            return match session.ensure_active() {
                Err(KernelError::SessionClosing) => {
                    session.execution.is_closed()
                }
                _ => Ok(true),
            };
        }
        // An error means the runtime was dropped, which also ends its shutdown.
        let waited = tokio::time::timeout(
            CLOSE_WAIT,
            closing.wait_for(|closing| !*closing),
        )
        .await;
        if waited.is_err() {
            tracing::warn!(
                "gave up waiting for Session {} to finish closing after {:?}",
                session_id,
                CLOSE_WAIT
            );
        }
        Ok(waited.is_ok())
    }
}
