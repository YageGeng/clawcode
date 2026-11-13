//! Closing and deleting Sessions after their active work has settled.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use protocol::SessionId;

use crate::runtime::{Kernel, KernelError};

impl Kernel {
    /// Cancels active work, waits for it to settle, and releases in-memory session resources.
    ///
    /// Runs to completion even when the caller stops awaiting; see [`Self::detached`].
    pub async fn close_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let kernel = self.clone();
        let session_id = session_id.clone();
        Self::detached(async move { kernel.perform_close(&session_id).await })
            .await
    }

    /// Releases an active runtime before removing its persisted session history idempotently.
    ///
    /// Runs to completion even when the caller stops awaiting; see [`Self::detached`].
    pub async fn delete_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let kernel = self.clone();
        let session_id = session_id.clone();
        Self::detached(async move { kernel.perform_delete(&session_id).await })
            .await
    }

    /// Runs one Session teardown on its own task inside the caller's span.
    ///
    /// Close and delete change the Session before their first await (they
    /// cancel its background work and request its Run's close), so a caller
    /// dropped midway would leave it half closed and refusing every later Run.
    /// That happens when an ACP client disconnects, and always when a Session
    /// asks to delete itself from its own tool call, which the close then kills.
    /// The task is never aborted, so a join error is its panic, re-raised here.
    async fn detached(
        teardown: impl Future<Output = Result<(), KernelError>> + Send + 'static,
    ) -> Result<(), KernelError> {
        match tokio::spawn(tracing::Instrument::in_current_span(teardown)).await
        {
            Ok(result) => result,
            Err(error) => std::panic::resume_unwind(error.into_panic()),
        }
    }

    /// Closes one Session in place; [`Self::close_session`] keeps it running to
    /// completion. Boxed as `Send` because the cascade recurses into children.
    fn perform_close<'a>(
        &'a self,
        session_id: &'a SessionId,
    ) -> BoxFuture<'a, Result<(), KernelError>> {
        Box::pin(async move {
            tracing::info!("started Session close {}", session_id);
            let result: Result<(), KernelError> = async {
                let session = self.session(session_id)?;
                session.background.shutdown.cancel();
                tracing::info!("abandoned subagent reports for closing session {}", session_id);
                let _run_guard = session.execution.acquire_close().await?;
                // No Run can start a process any more, so the table empties for good.
                if let Some(exec) = &session.exec {
                    let terminated = exec.terminate_all().await;
                    tracing::info!("terminated {} exec processes while closing session {}", terminated, session_id);
                    exec.remove_snapshot().await;
                }
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
                    let result = match self.perform_close(&child).await {
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
        })
    }

    /// Deletes one Session in place; [`Self::delete_session`] keeps it running to
    /// completion. Boxed as `Send` because the cascade recurses into children.
    fn perform_delete<'a>(
        &'a self,
        session_id: &'a SessionId,
    ) -> BoxFuture<'a, Result<(), KernelError>> {
        Box::pin(async move {
            tracing::info!("started Session deletion {}", session_id);
            let result: Result<(), KernelError> = async {
                // Reserve before closing so construction cannot claim the identifier
                // between runtime removal and persisted Store deletion.
                let _reservation = self.reserve_session_deletion(session_id)?;
                let (cwd, ownership) = match self.session(session_id) {
                    Ok(session) => (Some(session.cwd.clone()), session.ownership.clone()),
                    Err(KernelError::SessionNotFound(_)) => self
                        .store_factory
                        .header(session_id)?
                        .map_or((None, None), |header| (Some(header.cwd), header.ownership)),
                    Err(error) => return Err(error),
                };
                match self.perform_close(session_id).await {
                    Ok(()) | Err(KernelError::SessionNotFound(_)) => {}
                    Err(error) => return Err(error),
                }
                if let Some(cwd) = cwd {
                    for child in self.store_factory.headers(Some(&cwd))?.into_iter().filter(|item| item.ownership.as_ref().is_some_and(|edge| &edge.session_id == session_id)) {
                        tracing::info!("started cascading deletion of session {} owned by {}", child.id, session_id);
                        self.perform_delete(&child.id).await?;
                        tracing::info!("completed cascading deletion of session {} owned by {}", child.id, session_id);
                    }
                }
                self.store_factory.delete(session_id)?;
                // A deleted child leaves its live owner's roster at once.
                if let Some(edge) = ownership
                    && let Ok(owner) = self.session(&edge.session_id)
                {
                    owner.roster.forget_child(&edge.name)?;
                }
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
        })
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
