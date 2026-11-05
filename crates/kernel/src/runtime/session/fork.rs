//! Forking one Session's branch into a new persisted Session.

use std::path::PathBuf;

use protocol::{EntryId, IdKind, SessionId};
use store::{SessionCreateOptions, SessionForkOptions};

use crate::runtime::{Kernel, KernelError, mcp};

impl Kernel {
    /// Forks one selected ancestor branch into a new persisted session and registers it.
    pub async fn fork_session(
        &self,
        source_session_id: &SessionId,
        source_leaf: EntryId,
        new_session_id: SessionId,
        cwd: PathBuf,
    ) -> Result<PathBuf, KernelError> {
        let source = self.session(source_session_id)?;
        let context = self.extension_context(&source, None, None)?;
        let decision = source
            .extensions
            .runtime_ref()
            .emit_session_before_fork(
                &protocol::SessionBeforeForkEvent {
                    entry_id: source_leaf.clone(),
                    position: protocol::SessionForkPosition::At,
                },
                &context,
            )
            .await;
        if decision.cancel {
            return Err(KernelError::ExtensionBlocked(
                "session fork cancelled".to_string(),
            ));
        }
        let _reservation = self.reserve_session_build(&new_session_id)?;
        let source_path = source.transcript.path()?;
        let store = self.store_factory.fork(
            &source_path,
            SessionForkOptions {
                create: SessionCreateOptions {
                    session_id: new_session_id.clone(),
                    cwd: cwd.clone(),
                    parent_session_id: Some(source_session_id.clone()),
                },
                source_leaf,
                lane: source.transcript.lane(),
            },
        )?;
        let path = store.path().to_path_buf();
        let runtime = self
            .build_session(store, cwd, mcp::SessionMcpSource::Connect, None)
            .await?;
        self.register_session(new_session_id.clone(), &runtime)
            .await?;
        if let Err(error) = self
            .start_extensions(&runtime, protocol::SessionStartReason::Fork)
            .await
        {
            if let Err(rollback_error) = self
                .rollback_session_registration(&new_session_id, true)
                .await
            {
                tracing::warn!(
                    "failed to clean up forked Session {} after startup error: {}",
                    new_session_id,
                    rollback_error
                );
            }
            return Err(error);
        }
        self.checkpoint_started_session(&new_session_id, &runtime, true)
            .await?;
        Ok(path)
    }

    /// Generates a new session id and forks one selected ancestor branch.
    pub async fn fork_generated_session(
        &self,
        source_session_id: &SessionId,
        source_leaf: EntryId,
        cwd: PathBuf,
    ) -> Result<SessionId, KernelError> {
        let session_id =
            SessionId::try_from(self.id_generator.next(IdKind::Session))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.fork_session(
            source_session_id,
            source_leaf,
            session_id.clone(),
            cwd,
        )
        .await?;
        Ok(session_id)
    }
}
