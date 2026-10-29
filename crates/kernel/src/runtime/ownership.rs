use super::settings::SettingChange;
use super::*;
use protocol::{SessionOwnership, ToolSelection};
use store::OwnedSessionCreateOptions;

pub(in crate::runtime) const SUBAGENT_TOOL_NAMES: [&str; 2] =
    ["subagent", "background_subagent"];

/// Agent choices copied before a child's startup extensions observe it.
struct AgentSnapshot {
    model: Arc<dyn Model>,
    thinking_level: ThinkingLevel,
    tools: ToolSelection,
}

impl TryFrom<&Session> for AgentSnapshot {
    type Error = KernelError;
    /// Captures model and thinking atomically, together with the current tool policy.
    fn try_from(owner: &Session) -> Result<Self, Self::Error> {
        let (model, thinking_level) = owner.model.snapshot()?;
        Ok(Self {
            model,
            thinking_level,
            tools: owner.tools.snapshot()?.selection.clone(),
        })
    }
}

impl AgentSnapshot {
    /// Removes recursive delegation while preserving every other inherited choice.
    fn without_tools(mut self, names: &[&str]) -> Self {
        for name in names {
            self.tools.loaded.remove(*name);
            self.tools.disabled.insert((*name).into());
        }
        self
    }

    /// Uses the existing durable setting writers before publishing the child runtime.
    fn apply(
        &self,
        child: &Session,
        ids: &dyn IdGenerator,
    ) -> Result<(), KernelError> {
        for change in [
            SettingChange::Model {
                provider: self.model.profile().provider_id.clone(),
                model_id: self.model.profile().model_id.clone(),
            },
            SettingChange::ThinkingLevel(self.thinking_level),
        ] {
            let (kind, payload) = (&change).into();
            child.transcript.commit_setting(NewEntry {
                id: EntryId::try_from(ids.next(IdKind::Entry))
                    .map_err(|e| KernelError::Protocol(e.to_string()))?,
                kind,
                payload,
            })?;
        }
        child.model.replace_model(Arc::clone(&self.model))?;
        child.model.replace_thinking_level(self.thinking_level)?;
        tools::ToolSelectionUpdate::new(&child.tools, &child.transcript, ids)
            .adopt(self.tools.clone())
    }
}

/// Borrows the owner and Kernel for one transactional owned-session startup.
#[derive(typed_builder::TypedBuilder)]
struct OwnedSessionSpawn<'a> {
    kernel: &'a Kernel,
    owner: &'a Arc<Session>,
    ownership: SessionOwnership,
    title: SessionTitle,
}

impl OwnedSessionSpawn<'_> {
    /// Creates isolated history, adopts agent choices, and rolls back any failed startup.
    async fn create(self) -> Result<SessionId, KernelError> {
        // Close takes the write side after draining its Run, so externally started
        // children finish registration before the cascading child snapshot is taken.
        let _creation_guard = self.owner.owned_creation.read().await;
        self.owner.ensure_active()?;
        let snapshot = AgentSnapshot::try_from(self.owner.as_ref())?
            .without_tools(&SUBAGENT_TOOL_NAMES);
        let child =
            SessionId::try_from(self.kernel.id_generator.next(IdKind::Session))
                .map_err(|e| KernelError::Protocol(e.to_string()))?;
        let _reservation = self.kernel.reserve_session_build(&child)?;
        tracing::info!(
            "started owned Session creation {} for owner {}",
            child,
            self.ownership.owner()
        );
        let result: Result<(), KernelError> = async {
            let store = self.kernel.store_factory.create_owned(
                OwnedSessionCreateOptions {
                    create: SessionCreateOptions {
                        session_id: child.clone(),
                        cwd: self.owner.cwd.clone(),
                        parent_session_id: None,
                    },
                    ownership: self.ownership.clone(),
                },
            )?;
            let runtime = self
                .kernel
                .build_session(
                    store,
                    self.owner.cwd.clone(),
                    mcp::SessionMcpSource::Share(&self.owner.mcp),
                    Some(self.ownership.clone()),
                )
                .await?;
            // Every failure after the runtime exists but before registration must release it,
            // because registration rollback only cleans Sessions already in the map.
            let prepared = snapshot
                .apply(&runtime, self.kernel.id_generator.as_ref())
                .and_then(|()| self.owner.ensure_active());
            if let Err(error) = prepared {
                if let Err(cleanup_error) =
                    Kernel::release_unregistered_session(&runtime).await
                {
                    tracing::warn!(
                        "failed to release owned Session {} for owner {} before registration: {}",
                        child,
                        self.ownership.owner(),
                        cleanup_error
                    );
                }
                return Err(error);
            }
            self.kernel
                .register_session(child.clone(), &runtime)
                .await?;
            self.kernel
                .start_extensions(&runtime, protocol::SessionStartReason::New)
                .await?;
            self.kernel
                .checkpoint_started_session(&child, &runtime, true)
                .await?;
            self.kernel.rename_session(&child, self.title).await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::error!(
                "failed owned Session creation {} for owner {}: {}",
                child,
                self.ownership.owner(),
                error
            );
            if let Err(cleanup) = self
                .kernel
                .rollback_session_registration(&child, true)
                .await
            {
                tracing::warn!(
                    "failed cleanup of owned Session {}: {}",
                    child,
                    cleanup
                );
            }
            return Err(error);
        }
        tracing::info!(
            "completed owned Session creation {} for owner {}",
            child,
            self.ownership.owner()
        );
        Ok(child)
    }
}

impl Kernel {
    /// Waits for an existing owner startup instead of rebuilding it for each recovering child.
    pub(super) async fn resume_owner(
        &self,
        child: &SessionId,
        owner: &SessionId,
        cwd: &std::path::Path,
    ) -> Result<Option<Arc<Session>>, KernelError> {
        if let Ok(runtime) = self.session(owner) {
            return Ok(Some(runtime));
        }
        let sessions = self.store_factory.headers(Some(cwd))?;
        // A corrupt owner cycle must fail before waiting on this call's own reservation.
        let mut seen = HashSet::from([child]);
        let mut cursor = Some(owner);
        while let Some(id) = cursor {
            if !seen.insert(id) {
                tracing::error!(
                    "cyclic ownership while resuming session {} at owner {}",
                    child,
                    id
                );
                return Err(KernelError::Protocol(format!(
                    "cyclic session ownership at {id}"
                )));
            }
            cursor = sessions
                .iter()
                .find(|session| &session.id == id)
                .and_then(|session| session.ownership.as_ref())
                .map(SessionOwnership::owner);
        }
        loop {
            match self.session(owner) {
                Ok(runtime) => return Ok(Some(runtime)),
                Err(
                    KernelError::SessionNotFound(_)
                    | KernelError::SessionStarting,
                ) => {}
                Err(error) => return Err(error),
            }
            let pending = self
                .pending_sessions
                .lock()
                .map_err(|_error| KernelError::Poisoned)?
                .get(owner)
                .map(tokio::sync::watch::Sender::subscribe);
            if let Some(mut pending) = pending {
                tracing::info!(
                    "waiting for owner {} startup before resuming session {}",
                    owner,
                    child
                );
                // The reservation drops its sender on both success and rollback.
                let _ = pending.changed().await;
                continue;
            }
            // A cancelled startup may leave a Starting runtime without a reservation;
            // there is no producer to wait for or retry, so preserve its lifecycle error.
            match self.session(owner) {
                Ok(runtime) => return Ok(Some(runtime)),
                Err(KernelError::SessionNotFound(_)) => {}
                Err(error) => {
                    tracing::warn!(
                        "owner {} cannot resume child {}: {}",
                        owner,
                        child,
                        error
                    );
                    return Err(error);
                }
            }
            if !sessions.iter().any(|session| &session.id == owner) {
                tracing::warn!(
                    "owner {} of session {} is unavailable; connecting its own MCP",
                    owner,
                    child
                );
                return Ok(None);
            }
            tracing::info!("resuming owner {} before session {}", owner, child);
            match Box::pin(
                self.resume_session(owner.clone(), cwd.to_path_buf()),
            )
            .await
            {
                Ok(_) => {}
                Err(
                    KernelError::SessionStarting
                    | KernelError::DuplicateSession(_),
                ) => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Finds live owned children without maintaining a second mutable ownership index.
    pub(super) fn live_children(
        &self,
        owner: &SessionId,
    ) -> Result<Vec<SessionId>, KernelError> {
        // shortcut: scans live Sessions in O(n); add an owner index if runtime counts make this costly.
        Ok(self
            .sessions
            .read()
            .map_err(|_lock_error| KernelError::Poisoned)?
            .iter()
            .filter(|(_, child)| {
                child
                    .ownership
                    .as_ref()
                    .is_some_and(|edge| edge.owner() == owner)
            })
            .map(|(id, _)| id.clone())
            .collect())
    }

    /// Creates an isolated child with copied agent settings and a persistent owner edge.
    pub async fn create_owned_session(
        &self,
        ownership: SessionOwnership,
        title: SessionTitle,
    ) -> Result<SessionId, KernelError> {
        let owner = self.session(ownership.owner()).map_err(
            |error| match error {
                KernelError::SessionNotFound(_) => {
                    tracing::warn!(
                        "cannot create owned Session for unavailable owner {}",
                        ownership.owner()
                    );
                    KernelError::OwnerUnavailable(ownership.owner().clone())
                }
                error => error,
            },
        )?;
        OwnedSessionSpawn::builder()
            .kernel(self)
            .owner(&owner)
            .ownership(ownership)
            .title(title)
            .build()
            .create()
            .await
    }
}
