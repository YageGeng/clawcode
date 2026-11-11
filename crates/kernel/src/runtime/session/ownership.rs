use std::collections::HashSet;
use std::sync::Arc;

use protocol::{
    AgentMessage, EntryId, IdGenerator, IdKind, MessageIdentity, SessionId,
    SessionMode, SessionOwnership, SessionTitle, ThinkingLevel, ToolSelection,
    TurnId,
};
use store::{NewEntry, OwnedSessionCreateOptions, SessionCreateOptions};

use super::settings::SettingChange;
use crate::Model;
use crate::runtime::plan::ModeChange;
use crate::runtime::{Kernel, KernelError, Session, mcp, tools};

/// Agent tools only `/root` may call; children keep the mailbox tools.
pub(in crate::runtime) const ROOT_ONLY_AGENT_TOOLS: [&str; 3] = [
    protocol::SPAWN_AGENT_TOOL,
    protocol::FOLLOWUP_TASK_TOOL,
    protocol::INTERRUPT_AGENT_TOOL,
];

/// Agent choices copied before a child's startup extensions observe it.
struct AgentSnapshot {
    model: Arc<dyn Model>,
    thinking_level: ThinkingLevel,
    tools: ToolSelection,
    /// A child started in plan mode stays read-only, or it could edit for its owner.
    mode: SessionMode,
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
            mode: owner.plan.mode()?,
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
        if self.mode == SessionMode::Plan {
            let change = ModeChange {
                mode: self.mode,
                turn_id: TurnId::try_from(ids.next(IdKind::Turn))
                    .map_err(|e| KernelError::Protocol(e.to_string()))?,
            };
            child.transcript.append_mode_note(
                EntryId::try_from(ids.next(IdKind::Entry))
                    .map_err(|e| KernelError::Protocol(e.to_string()))?,
                &change,
            )?;
            child.plan.replace_mode(self.mode)?;
        }
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
    /// A configured model replacing the inherited one, resolved by the caller.
    #[builder(default)]
    model: Option<Arc<dyn Model>>,
    /// A thinking level replacing the inherited one.
    #[builder(default)]
    thinking_level: Option<ThinkingLevel>,
    /// Owner history the child inherits before its first task, already filtered.
    #[builder(default)]
    history: Vec<AgentMessage>,
}

/// Everything one owned child is created from (pi `createConversation({ ownership })`).
#[derive(typed_builder::TypedBuilder)]
pub struct OwnedSessionRequest {
    /// Owner edge written into the child's header.
    pub ownership: SessionOwnership,
    /// Child display title.
    pub title: SessionTitle,
    /// Model replacing the inherited one.
    #[builder(default)]
    pub model: Option<Arc<dyn Model>>,
    /// Thinking level replacing the inherited one.
    #[builder(default)]
    pub thinking_level: Option<ThinkingLevel>,
    /// Filtered owner history copied in order before the child starts.
    #[builder(default)]
    pub history: Vec<AgentMessage>,
}

impl OwnedSessionSpawn<'_> {
    /// Copies inherited messages under fresh identities so the child's store
    /// never shares message ids with its owner.
    fn write_history(&self, child: &Session) -> Result<(), KernelError> {
        for message in &self.history {
            let copy = AgentMessage {
                identity: MessageIdentity {
                    message_id: self.kernel.message_id()?,
                    turn_id: message.identity.turn_id.clone(),
                },
                ..message.clone()
            };
            self.kernel.persist_message(child, &copy)?;
        }
        Ok(())
    }

    /// Creates isolated history, adopts agent choices, and rolls back any failed startup.
    async fn create(self) -> Result<SessionId, KernelError> {
        // Close takes the write side after draining its Run, so externally started
        // children finish registration before the cascading child snapshot is taken.
        let _creation_guard = self.owner.owned_creation.read().await;
        self.owner.ensure_active()?;
        let mut snapshot = AgentSnapshot::try_from(self.owner.as_ref())?
            .without_tools(&ROOT_ONLY_AGENT_TOOLS);
        if let Some(model) = &self.model {
            snapshot.model = Arc::clone(model);
        }
        if let Some(level) = self.thinking_level {
            snapshot.thinking_level = level;
        }
        let child =
            SessionId::try_from(self.kernel.id_generator.next(IdKind::Session))
                .map_err(|e| KernelError::Protocol(e.to_string()))?;
        let _reservation = self.kernel.reserve_session_build(&child)?;
        tracing::info!(
            "started owned Session creation {} for owner {}",
            child,
            self.ownership.session_id
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
                .and_then(|()| self.write_history(&runtime))
                .and_then(|()| self.owner.ensure_active());
            if let Err(error) = prepared {
                if let Err(cleanup_error) =
                    Kernel::release_unregistered_session(&runtime).await
                {
                    tracing::warn!(
                        "failed to release owned Session {} for owner {} before registration: {}",
                        child,
                        self.ownership.session_id,
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
            self.kernel
                .rename_session(&child, self.title.clone())
                .await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::error!(
                "failed owned Session creation {} for owner {}: {}",
                child,
                self.ownership.session_id,
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
            self.ownership.session_id
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
                .map(|edge| &edge.session_id);
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
            if self.wait_for_session_reservation(owner).await? {
                tracing::info!(
                    "waited for owner {} startup before resuming session {}",
                    owner,
                    child
                );
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
            .read()?
            .iter()
            .filter(|(_, child)| {
                child
                    .ownership
                    .as_ref()
                    .is_some_and(|edge| &edge.session_id == owner)
            })
            .map(|(id, _)| id.clone())
            .collect())
    }

    /// Creates an isolated child with copied agent settings and a persistent owner edge.
    /// Model and thinking overrides replace inherited choices; tools stay inherited.
    pub async fn create_owned_session(
        &self,
        request: OwnedSessionRequest,
    ) -> Result<SessionId, KernelError> {
        let OwnedSessionRequest {
            ownership,
            title,
            model,
            thinking_level,
            history,
        } = request;
        let owner = self.session(&ownership.session_id).map_err(|error| {
            match error {
                KernelError::SessionNotFound(_) => {
                    tracing::warn!(
                        "cannot create owned Session for unavailable owner {}",
                        ownership.session_id
                    );
                    KernelError::OwnerUnavailable(ownership.session_id.clone())
                }
                error => error,
            }
        })?;
        OwnedSessionSpawn::builder()
            .kernel(self)
            .owner(&owner)
            .ownership(ownership)
            .title(title)
            .model(model)
            .thinking_level(thinking_level)
            .history(history)
            .build()
            .create()
            .await
    }
}
