//! Creating and resuming Sessions, including their startup hooks and checkpoint.

use std::path::PathBuf;
use std::sync::Arc;

use protocol::{IdKind, SessionId};
use store::SessionCreateOptions;

use super::resources::SessionResources;
use crate::runtime::{Kernel, KernelError, Session, mcp};

impl Kernel {
    /// Generates, persists, and registers one new session for a working directory.
    pub async fn create_generated_session(
        &self,
        cwd: PathBuf,
    ) -> Result<SessionId, KernelError> {
        let session_id =
            SessionId::try_from(self.id_generator.next(IdKind::Session))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        self.create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await?;
        Ok(session_id)
    }

    /// Creates a persisted session with a root `main` lane and registers it.
    pub async fn create_session(
        &self,
        options: SessionCreateOptions,
    ) -> Result<PathBuf, KernelError> {
        let session_id = options.session_id.clone();
        let cwd = options.cwd.clone();
        tracing::info!("started Session creation {}", session_id);
        let result: Result<PathBuf, KernelError> = async {
            let _reservation = self.reserve_session_build(&session_id)?;
            let store = self.store_factory.create(options)?;
            let path = store.path().to_path_buf();
            let runtime = self
                .build_session(store, cwd, mcp::SessionMcpSource::Connect, None)
                .await?;
            self.register_session(session_id.clone(), &runtime).await?;
            if let Err(error) = self
                .start_extensions(&runtime, protocol::SessionStartReason::New)
                .await
            {
                if let Err(rollback_error) =
                    self.rollback_session_registration(&session_id, true).await
                {
                    tracing::warn!(
                        "failed to clean up Session {} after startup error: {}",
                        session_id,
                        rollback_error
                    );
                }
                return Err(error);
            }
            self.checkpoint_started_session(&session_id, &runtime, true)
                .await?;
            Ok(path)
        }
        .await;
        match result {
            Ok(path) => {
                tracing::info!("completed Session creation {}", session_id);
                Ok(path)
            }
            Err(error) => {
                tracing::error!(
                    "failed Session creation {}: {}",
                    session_id,
                    error
                );
                Err(error)
            }
        }
    }

    /// Validates a live Session attachment without waiting for its operation gate.
    pub fn validate_session_attachment(
        &self,
        session_id: &SessionId,
        cwd: &std::path::Path,
    ) -> Result<protocol::SessionRuntimeSnapshot, KernelError> {
        let session = self.session(session_id).map_err(|error| {
            tracing::warn!(
                "failed to validate attachment for Session {}: {}",
                session_id,
                error
            );
            error
        })?;
        if session.cwd.as_path() != cwd {
            let error = KernelError::SessionCwdMismatch {
                session_id: session_id.clone(),
                expected: session.cwd.clone(),
                received: cwd.to_path_buf(),
            };
            tracing::warn!(
                "failed to validate attachment for Session {}: {}",
                session_id,
                error
            );
            return Err(error);
        }
        let running = session.execution.active_run().map_err(|error| {
            tracing::warn!(
                "failed to read active Run while validating attachment for Session {}: {}",
                session_id,
                error
            );
            error
        })?;
        Ok(protocol::SessionRuntimeSnapshot {
            session_id: session_id.clone(),
            running: running.is_some(),
        })
    }

    /// Opens one persisted session or reuses the matching live runtime idempotently.
    pub async fn resume_session(
        &self,
        session_id: SessionId,
        cwd: PathBuf,
    ) -> Result<PathBuf, KernelError> {
        tracing::info!("started Session Resume {}", session_id);
        let result: Result<PathBuf, KernelError> = async {
        let existing =
            self.sessions.read()?.get(&session_id).map(Arc::clone);
        if let Some(existing) = existing {
            // A live map entry is published during startup for hook context, but
            // external Resume must not observe it as an initialized Session.
            existing.ensure_active()?;
            if existing.cwd != cwd {
                return Err(KernelError::SessionCwdMismatch {
                    session_id: session_id.clone(),
                    expected: existing.cwd.clone(),
                    received: cwd,
                });
            }
            let path = existing.transcript.path()?;
            let context = self.extension_context(&existing, None, None)?;
            let before = existing
                .extensions
                .runtime_ref()
                .emit_session_before_switch(
                    &protocol::SessionBeforeSwitchEvent {
                        reason: protocol::SessionSwitchReason::Resume,
                        target_session_id: Some(session_id.clone()),
                    },
                    &context,
                )
                .await;
            // The context contains an owned Session snapshot and is no longer
            // needed after the hook; release it before adding another await so
            // the Resume future does not retain that snapshot in its state.
            drop(context);
            if before.cancel {
                return Err(KernelError::ExtensionBlocked(
                    "session resume cancelled".to_string(),
                ));
            }
            // The hook stays outside the operation gate so Extension Host calls
            // can reenter safely. Acquiring afterward rejects a Resume when
            // Close won the lifecycle race while the hook was suspended.
            // A live Run already owns the gate. Attaching its projection must not
            // wait for the answer, especially when that answer needs an elicitation.
            let _operation_guard = if existing.execution.active_run()?.is_some() {
                existing.ensure_active()?;
                None
            } else {
                Some(existing.acquire_operation().await?)
            };
            existing.sync_store()?;
            tracing::info!("completed Session Resume {} using live runtime", session_id);
            return Ok(path);
        }
        let _reservation = self.reserve_session_build(&session_id)?;
        let metadata = self
            .store_factory
            .headers(Some(&cwd))?
            .into_iter()
            .find(|metadata| metadata.id == session_id)
            .ok_or_else(|| KernelError::SessionNotFound(session_id.clone()))?;
        let store = self.store_factory.open(&metadata.path)?;
        let path = store.path().to_path_buf();
        let owner = match &metadata.ownership {
            Some(edge) => self.resume_owner(&session_id, &edge.session_id, &cwd).await?,
            None => None,
        };
        let _owner_creation = match &owner {
            Some(owner) => {
                let guard = owner.owned_creation.read().await;
                owner.ensure_active()?;
                Some(guard)
            }
            None => None,
        };
        let source = owner.as_ref().map_or(mcp::SessionMcpSource::Connect, |runtime| mcp::SessionMcpSource::Share(&runtime.mcp));
        let runtime = self.build_session(store, cwd, source, metadata.ownership).await?;
        let context = self.extension_context(&runtime, None, None)?;
        let before = runtime
            .extensions
            .runtime_ref()
            .emit_session_before_switch(
                &protocol::SessionBeforeSwitchEvent {
                    reason: protocol::SessionSwitchReason::Resume,
                    target_session_id: Some(session_id.clone()),
                },
                &context,
            )
            .await;
        if before.cancel {
            if let Err(error) =
                Self::release_unregistered_session(&runtime).await
            {
                // Preserve the extension cancellation as the operation result
                // while keeping cleanup failures visible to operators.
                tracing::warn!(
                    "failed to clean up MCP after Session Resume cancellation: {}",
                    error
                );
            }
            return Err(KernelError::ExtensionBlocked(
                "session resume cancelled".to_string(),
            ));
        }
        self.register_session(session_id.clone(), &runtime).await?;
        if let Err(error) = self
            .start_extensions(&runtime, protocol::SessionStartReason::Resume)
            .await
        {
            if let Err(rollback_error) =
                self.rollback_session_registration(&session_id, false).await
            {
                tracing::warn!(
                    "failed to clean up Session {} after Resume startup error: {}",
                    session_id,
                    rollback_error
                );
            }
            return Err(error);
        }
        self.checkpoint_started_session(&session_id, &runtime, false)
            .await?;
        tracing::info!("completed Session Resume {} from persisted Store", session_id);
        Ok(path)
        }
        .await;
        if let Err(error) = &result {
            tracing::error!("failed Session Resume {}: {}", session_id, error);
        }
        result
    }

    /// Runs typed startup, resource, session, model, and thinking points for one runtime.
    pub(in crate::runtime) async fn start_extensions(
        &self,
        session: &Arc<Session>,
        reason: protocol::SessionStartReason,
    ) -> Result<(), KernelError> {
        let session_id = session.transcript.session_id()?;
        tracing::debug!(
            "started Session extension startup {} with reason {:?}",
            session_id,
            reason
        );
        let result: Result<(), KernelError> = async {
            let context = self.extension_context(session, None, None)?;
            let trust = session
                .extensions
                .runtime_ref()
                .emit_project_trust(
                    &protocol::ProjectTrustEvent {
                        cwd: session.cwd.clone(),
                    },
                    &context,
                )
                .await;
            // Pi announces the live session before extensions discover resources
            // that will be attached to that session runtime.
            session
                .extensions
                .runtime_ref()
                .emit_session_start(
                    &protocol::SessionStartEvent {
                        reason,
                        previous_session_id: None,
                    },
                    &context,
                )
                .await;
            let project_resources_allowed =
                trust.trusted != protocol::ProjectTrustDecision::No;
            // Project trust gates built-in project roots; Extension-owned roots
            // remain discoverable because they are not implicitly project content.
            let resources = session
                .extensions
                .runtime_ref()
                .emit_resources_discover(
                    &protocol::ResourcesDiscoverEvent {
                        cwd: session.cwd.clone(),
                        reason: protocol::ResourcesDiscoverReason::Startup,
                    },
                    &context,
                )
                .await;
            let skills = self
                .skill_factory
                .as_ref()
                .map(|factory| {
                    factory.create(protocol::SkillResourceRequest {
                        cwd: session.cwd.clone(),
                        project_resources_allowed,
                        extension_skill_paths: resources.skill_paths,
                    })
                })
                .transpose()?
                .map(Arc::new);
            let prompt = self.prompt_factory.create(
                protocol::PromptResourceRequest {
                    cwd: session.cwd.clone(),
                    project_resources_allowed,
                    extension_prompt_paths: resources.prompt_paths,
                },
            )?;
            // Publish Prompt and Skill resources together so readers cannot observe
            // a partially initialized startup snapshot.
            session
                .resources
                .set(SessionResources::new(Arc::new(prompt), skills))
                .map_err(|_resources| {
                    KernelError::Protocol(
                        "session resources already initialized".to_string(),
                    )
                })?;
            let (restored_model, restored_thinking_level) =
                session.model.snapshot()?;
            let restored_model = restored_model.profile().clone();
            session
                .extensions
                .runtime_ref()
                .emit_model_select(
                    &protocol::ModelSelectEvent {
                        model: restored_model,
                        previous_model: None,
                        source: protocol::ModelSelectSource::Restore,
                    },
                    &context,
                )
                .await;
            session
                .extensions
                .runtime_ref()
                .emit_thinking_level_select(
                    &protocol::ThinkingLevelSelectEvent {
                        level: restored_thinking_level,
                        // A session without a persisted choice starts here, so it is no change.
                        previous_level: protocol::ThinkingLevel::default(),
                    },
                    &context,
                )
                .await;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                tracing::debug!(
                    "completed Session extension startup {} with reason {:?}",
                    session_id,
                    reason
                );
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    "failed Session extension startup {} with reason {:?}: {}",
                    session_id,
                    reason,
                    error
                );
                Err(error)
            }
        }
    }

    /// Syncs startup writes or removes the partially started runtime on failure.
    pub(in crate::runtime) async fn checkpoint_started_session(
        &self,
        session_id: &SessionId,
        runtime: &Arc<Session>,
        delete_persisted: bool,
    ) -> Result<(), KernelError> {
        // Activation follows the durable startup checkpoint so no public
        // operation can observe resources that may still be rolled back.
        let checkpoint = runtime
            .sync_store()
            .and_then(|()| runtime.finish_starting());
        if let Err(error) = checkpoint {
            tracing::error!(
                "failed to checkpoint started session {}; rolling back registration: {}",
                session_id,
                error
            );
            if let Err(rollback_error) = self
                .rollback_session_registration(session_id, delete_persisted)
                .await
            {
                tracing::warn!(
                    "failed to clean up Session {} after checkpoint error: {}",
                    session_id,
                    rollback_error
                );
            }
            return Err(error);
        }
        Ok(())
    }
}
