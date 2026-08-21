use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v2 as wire;
use async_trait::async_trait;
use kernel::{EventSink, Kernel, SinkError};
use protocol::{
    AgentEvent, AgentEventPayload, RunId, Sequence, SessionId,
    SessionReplayItem,
};
use tokio::sync::broadcast;

use crate::AcpEventMapper;
use crate::recovery::{
    GroupMeta, OperationPhase, RecoveryMode, RecoveryPlan, SessionCursor,
};

/// All ACP notifications mapped from one indivisible Kernel event.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub(crate) struct EventGroup {
    /// Recoverable Operation owning this event, when one is active.
    #[builder(default, setter(strip_option))]
    pub(crate) run_id: Option<RunId>,
    /// Monotonic Kernel event sequence shared by every mapped update.
    pub(crate) sequence: Sequence,
    /// Existing ACP event correlation metadata.
    pub(crate) metadata: wire::Meta,
    /// ACP updates in mapper output order.
    pub(crate) updates: Vec<wire::SessionUpdate>,
    /// Explicit boundary for a recoverable Operation journal.
    #[builder(default, setter(strip_option))]
    pub(crate) operation_phase: Option<OperationPhase>,
    /// Internal Operation kind used to distinguish nested compaction boundaries.
    #[builder(default, setter(strip_option))]
    operation_kind: Option<OperationKind>,
}

/// Recoverable top-level Operation owning one journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationKind {
    Run,
    SlashCommand,
    Compaction,
}

impl EventGroup {
    /// Builds ordered ACP notifications with an atomic projection-group envelope.
    pub(crate) fn notifications(
        &self,
        session_id: &SessionId,
        namespace: &str,
    ) -> Result<Vec<wire::UpdateSessionNotification>, ProjectionError> {
        let projection_count = self.updates.len();
        self.updates
            .iter()
            .cloned()
            .enumerate()
            .map(|(projection_index, update)| {
                let mut metadata = self.metadata.clone();
                if let Some(run_id) = &self.run_id {
                    let builder = GroupMeta::builder()
                        .run_id(run_id.clone())
                        .projection_index(projection_index)
                        .projection_count(projection_count);
                    let recovery = match self.operation_phase {
                        Some(operation_phase) => builder
                            .operation_phase(operation_phase)
                            .build(),
                        None => builder.build(),
                    };
                    let product = metadata
                        .entry(namespace.to_string())
                        .or_insert_with(|| serde_json::json!({}));
                    let product = product.as_object_mut().ok_or_else(|| {
                        ProjectionError::Metadata(format!(
                            "ACP metadata namespace {namespace} is not an object"
                        ))
                    })?;
                    product.insert(
                        "sessionRecovery".to_string(),
                        serde_json::to_value(recovery).map_err(|error| {
                            ProjectionError::Metadata(error.to_string())
                        })?,
                    );
                }
                Ok(wire::UpdateSessionNotification::new(
                    session_id.to_string(),
                    update,
                )
                .meta(metadata))
            })
            .collect()
    }
}

/// Recoverable state retained for the current or most recently completed Operation.
#[derive(typed_builder::TypedBuilder)]
struct OperationJournal {
    /// Stable Operation identifier.
    run_id: RunId,
    /// Top-level lifecycle whose terminal event closes this journal.
    operation_kind: OperationKind,
    /// Durable Session projection captured before the start event.
    baseline: Vec<SessionReplayItem>,
    /// Ordered groups projected since the Operation started.
    groups: Vec<Arc<EventGroup>>,
    /// Whether this Operation can still append events.
    running: bool,
    /// Whether an incremental cursor can safely address this journal.
    incremental_available: bool,
}

/// Mutable journal state protected independently for each Session.
#[derive(Default)]
struct ProjectionState {
    journal: Option<OperationJournal>,
}

/// Failures while retaining or replaying one Session projection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProjectionError {
    /// One Session projection lock was poisoned.
    #[error("ACP Session projection lock poisoned")]
    Poisoned,
    /// The requested Operation journal is unavailable.
    #[error("ACP Operation projection journal is unavailable")]
    CursorUnavailable,
    /// ACP projection metadata could not be represented on the wire.
    #[error("invalid ACP projection metadata: {0}")]
    Metadata(String),
}

/// Atomic journal snapshot plus the live receiver created before that snapshot.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct ProjectionFeed {
    /// Durable Session projection preceding the retained Operation.
    pub(crate) baseline: Vec<SessionReplayItem>,
    /// Retained event groups at or after the requested sequence.
    pub(crate) backlog: Vec<Arc<EventGroup>>,
    /// Receiver for groups published after subscription began.
    pub(crate) receiver: broadcast::Receiver<Arc<EventGroup>>,
    /// Operation journal attached by this subscription.
    pub(crate) run_id: RunId,
    /// Inclusive sequence requested by the client.
    pub(crate) next_sequence: Sequence,
    /// Last sequence present when the snapshot was captured.
    pub(crate) tail_sequence: Sequence,
    /// Whether the Operation was still active at snapshot time.
    pub(crate) running: bool,
}

/// Exclusive source selected for a durable replay after its Store snapshot.
pub(crate) enum DurableResumeSelection {
    /// A newly available journal supersedes the captured Store snapshot.
    Retained(ProjectionFeed),
    /// No journal existed, so the captured Store snapshot precedes the live stream.
    Live {
        replay: Vec<SessionReplayItem>,
        receiver: broadcast::Receiver<Arc<EventGroup>>,
    },
}

/// One Session's recoverable journal and connection-independent broadcast stream.
pub(crate) struct SessionProjection {
    state: Mutex<ProjectionState>,
    sender: broadcast::Sender<Arc<EventGroup>>,
}

impl SessionProjection {
    /// Creates one bounded Session projection broadcast stream.
    pub(crate) fn new(capacity: usize) -> Self {
        let (sender, _receiver) = broadcast::channel(capacity);
        Self {
            state: Mutex::new(ProjectionState::default()),
            sender,
        }
    }

    /// Creates a live receiver without changing retained journal state.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Arc<EventGroup>> {
        self.sender.subscribe()
    }

    /// Appends one group to its Operation journal before broadcasting it.
    pub(crate) fn append_group(
        &self,
        group: Arc<EventGroup>,
        baseline: Vec<SessionReplayItem>,
    ) -> Result<(), ProjectionError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_poison_error| ProjectionError::Poisoned)?;
            if group.operation_phase == Some(OperationPhase::Start) {
                let run_id = group
                    .run_id
                    .clone()
                    .ok_or(ProjectionError::CursorUnavailable)?;
                state.journal = Some(
                    OperationJournal::builder()
                        .run_id(run_id)
                        .operation_kind(
                            group.operation_kind.unwrap_or(OperationKind::Run),
                        )
                        .baseline(baseline)
                        .groups(vec![Arc::clone(&group)])
                        .running(true)
                        .incremental_available(true)
                        .build(),
                );
            } else if let Some(journal) = state.journal.as_mut()
                && journal.running
                && group.run_id.as_ref() == Some(&journal.run_id)
            {
                journal.groups.push(Arc::clone(&group));
                if group.operation_phase == Some(OperationPhase::End) {
                    journal.running = false;
                }
            }
        }
        // A missing receiver is expected while every ACP transport is offline.
        let _send_result = self.sender.send(group);
        Ok(())
    }

    /// Returns retained groups from one inclusive Operation event cursor.
    pub(crate) fn snapshot_from(
        &self,
        run_id: &RunId,
        sequence: Sequence,
    ) -> Result<Vec<Arc<EventGroup>>, ProjectionError> {
        let state = self
            .state
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?;
        let journal = state
            .journal
            .as_ref()
            .filter(|journal| {
                &journal.run_id == run_id && journal.incremental_available
            })
            .ok_or(ProjectionError::CursorUnavailable)?;
        let head = journal
            .groups
            .first()
            .map(|group| group.sequence.get())
            .ok_or(ProjectionError::CursorUnavailable)?;
        let tail = journal
            .groups
            .last()
            .map(|group| group.sequence.get())
            .ok_or(ProjectionError::CursorUnavailable)?;
        if sequence.get() < head || sequence.get() > tail.saturating_add(1) {
            return Err(ProjectionError::CursorUnavailable);
        }
        Ok(journal
            .groups
            .iter()
            .filter(|group| group.sequence >= sequence)
            .map(Arc::clone)
            .collect())
    }

    /// Atomically attaches a receiver and snapshots one inclusive journal cursor.
    pub(crate) fn subscribe_from_cursor(
        &self,
        run_id: &RunId,
        sequence: Sequence,
    ) -> Result<ProjectionFeed, ProjectionError> {
        // Subscribe before taking the snapshot so concurrent appends can only
        // overlap the backlog; the watcher removes that overlap by sequence.
        let receiver = self.sender.subscribe();
        let state = self
            .state
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?;
        let journal = state
            .journal
            .as_ref()
            .filter(|journal| {
                &journal.run_id == run_id && journal.incremental_available
            })
            .ok_or(ProjectionError::CursorUnavailable)?;
        let head = journal
            .groups
            .first()
            .map(|group| group.sequence.get())
            .ok_or(ProjectionError::CursorUnavailable)?;
        let tail_sequence = journal
            .groups
            .last()
            .map(|group| group.sequence)
            .ok_or(ProjectionError::CursorUnavailable)?;
        if sequence.get() < head
            || sequence.get() > tail_sequence.get().saturating_add(1)
        {
            return Err(ProjectionError::CursorUnavailable);
        }
        Ok(ProjectionFeed::builder()
            .baseline(Vec::new())
            .backlog(
                journal
                    .groups
                    .iter()
                    .filter(|group| group.sequence >= sequence)
                    .map(Arc::clone)
                    .collect(),
            )
            .receiver(receiver)
            .run_id(journal.run_id.clone())
            .next_sequence(sequence)
            .tail_sequence(tail_sequence)
            .running(journal.running)
            .build())
    }

    /// Atomically attaches a receiver and snapshots the full retained Operation.
    pub(crate) fn subscribe_from_start(
        &self,
    ) -> Result<ProjectionFeed, ProjectionError> {
        let receiver = self.sender.subscribe();
        let state = self
            .state
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?;
        let journal = state
            .journal
            .as_ref()
            .ok_or(ProjectionError::CursorUnavailable)?;
        let next_sequence = journal
            .groups
            .first()
            .map(|group| group.sequence)
            .ok_or(ProjectionError::CursorUnavailable)?;
        let tail_sequence = journal
            .groups
            .last()
            .map(|group| group.sequence)
            .ok_or(ProjectionError::CursorUnavailable)?;
        Ok(ProjectionFeed::builder()
            .baseline(journal.baseline.clone())
            .backlog(journal.groups.iter().map(Arc::clone).collect())
            .receiver(receiver)
            .run_id(journal.run_id.clone())
            .next_sequence(next_sequence)
            .tail_sequence(tail_sequence)
            .running(journal.running)
            .build())
    }

    /// Returns the active Operation identifier used by events without one in their payload.
    fn active_operation(
        &self,
    ) -> Result<Option<(RunId, OperationKind)>, ProjectionError> {
        let state = self
            .state
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?;
        Ok(state
            .journal
            .as_ref()
            .filter(|journal| journal.running)
            .map(|journal| (journal.run_id.clone(), journal.operation_kind)))
    }

    /// Selects incremental watch only when the retained journal covers the cursor.
    fn recovery_plan(
        &self,
        cursor: &SessionCursor,
    ) -> Result<RecoveryPlan, ProjectionError> {
        let state = self
            .state
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?;
        let Some(journal) = state.journal.as_ref() else {
            return Ok(RecoveryPlan::builder()
                .session_id(cursor.session_id.clone())
                .mode(RecoveryMode::Resume)
                .running(false)
                .build());
        };
        let covered = journal.incremental_available
            && journal.run_id == cursor.run_id
            && journal
                .groups
                .first()
                .is_some_and(|group| cursor.next_sequence >= group.sequence)
            && journal.groups.last().is_some_and(|group| {
                cursor.next_sequence.get()
                    <= group.sequence.get().saturating_add(1)
            });
        if covered {
            Ok(RecoveryPlan::builder()
                .session_id(cursor.session_id.clone())
                .mode(RecoveryMode::Watch)
                .run_id(cursor.run_id.clone())
                .next_sequence(cursor.next_sequence)
                .running(journal.running)
                .build())
        } else {
            Ok(RecoveryPlan::builder()
                .session_id(cursor.session_id.clone())
                .mode(RecoveryMode::Resume)
                .running(journal.running)
                .build())
        }
    }

    /// Returns whether an Operation journal is currently retained.
    fn has_journal(&self) -> Result<bool, ProjectionError> {
        Ok(self
            .state
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?
            .journal
            .is_some())
    }

    /// Publishes an ungrouped Idle fallback and disables incremental recovery.
    fn publish_terminal_fallback(&self) -> Result<(), ProjectionError> {
        let sequence = {
            let mut state = self
                .state
                .lock()
                .map_err(|_poison_error| ProjectionError::Poisoned)?;
            let next = state
                .journal
                .as_ref()
                .and_then(|journal| journal.groups.last())
                .map_or(1, |group| group.sequence.get().saturating_add(1));
            if let Some(journal) = state.journal.as_mut() {
                journal.running = false;
                journal.incremental_available = false;
            }
            Sequence::try_from(next)
                .map_err(|error| ProjectionError::Metadata(error.to_string()))?
        };
        let terminal = EventGroup::builder()
            .sequence(sequence)
            .metadata(wire::Meta::default())
            .updates(vec![wire::SessionUpdate::StateUpdate(
                wire::StateUpdate::Idle(wire::IdleStateUpdate::new()),
            )])
            .build();
        let _send_result = self.sender.send(Arc::new(terminal));
        Ok(())
    }
}

/// Factory-wide registry retaining one projection stream per Kernel Session.
pub(crate) struct ProjectionRegistry {
    kernel: Arc<Kernel>,
    sessions: Mutex<BTreeMap<SessionId, Arc<SessionProjection>>>,
}

impl ProjectionRegistry {
    /// Creates an empty projection registry for one shared Kernel.
    pub(crate) fn new(kernel: Arc<Kernel>) -> Self {
        Self {
            kernel,
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    /// Creates a connection-independent event sink for one Session.
    pub(crate) fn sink(
        self: &Arc<Self>,
        session_id: SessionId,
    ) -> Arc<dyn EventSink> {
        Arc::new(ProjectionSink {
            session_id,
            registry: Arc::clone(self),
        })
    }

    /// Subscribes to future projected groups for one Session.
    pub(crate) fn subscribe_live(
        &self,
        session_id: &SessionId,
    ) -> Result<broadcast::Receiver<Arc<EventGroup>>, ProjectionError> {
        Ok(self.projection(session_id)?.subscribe())
    }

    /// Atomically subscribes from one inclusive Operation cursor.
    pub(crate) fn subscribe_from_cursor(
        &self,
        session_id: &SessionId,
        run_id: &RunId,
        sequence: Sequence,
    ) -> Result<ProjectionFeed, ProjectionError> {
        self.projection(session_id)?
            .subscribe_from_cursor(run_id, sequence)
    }

    /// Reads retained groups for watcher lag compensation.
    pub(crate) fn snapshot_from(
        &self,
        session_id: &SessionId,
        run_id: &RunId,
        sequence: Sequence,
    ) -> Result<Vec<Arc<EventGroup>>, ProjectionError> {
        self.projection(session_id)?.snapshot_from(run_id, sequence)
    }

    /// Atomically subscribes from the retained Operation baseline.
    pub(crate) fn subscribe_from_start(
        &self,
        session_id: &SessionId,
    ) -> Result<ProjectionFeed, ProjectionError> {
        self.projection(session_id)?.subscribe_from_start()
    }

    /// Selects either a retained journal or an earlier durable snapshot, never both.
    pub(crate) fn select_durable_resume(
        &self,
        session_id: &SessionId,
        receiver: broadcast::Receiver<Arc<EventGroup>>,
        replay: Vec<SessionReplayItem>,
    ) -> Result<DurableResumeSelection, ProjectionError> {
        match self.subscribe_from_start(session_id) {
            Ok(subscription) => {
                Ok(DurableResumeSelection::Retained(subscription))
            }
            Err(ProjectionError::CursorUnavailable) => {
                Ok(DurableResumeSelection::Live { replay, receiver })
            }
            Err(error) => Err(error),
        }
    }

    /// Returns whether a Session has a recoverable current or completed journal.
    pub(crate) fn has_journal(
        &self,
        session_id: &SessionId,
    ) -> Result<bool, ProjectionError> {
        let projection = self
            .sessions
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?
            .get(session_id)
            .map(Arc::clone);
        projection.map_or(Ok(false), |projection| projection.has_journal())
    }

    /// Drops retained projection state after Session close or deletion.
    pub(crate) fn remove(
        &self,
        session_id: &SessionId,
    ) -> Result<(), ProjectionError> {
        let removed = self
            .sessions
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?
            .remove(session_id);
        if removed.is_some() {
            tracing::info!("removed ACP projection for Session {}", session_id);
        }
        Ok(())
    }

    /// Selects the recovery plan for one Initialize cursor.
    pub(crate) fn recovery_plan(
        &self,
        cursor: &SessionCursor,
    ) -> Result<RecoveryPlan, ProjectionError> {
        let projection = self
            .sessions
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?
            .get(&cursor.session_id)
            .map(Arc::clone);
        match projection {
            Some(projection) => projection.recovery_plan(cursor),
            None => Ok(RecoveryPlan::builder()
                .session_id(cursor.session_id.clone())
                .mode(RecoveryMode::Resume)
                .running(false)
                .build()),
        }
    }

    /// Ends the live UI state after a fatal Kernel error without claiming a complete cursor.
    pub(crate) fn publish_terminal_fallback(
        &self,
        session_id: &SessionId,
    ) -> Result<(), ProjectionError> {
        self.projection(session_id)?.publish_terminal_fallback()
    }

    /// Resolves or creates one Session projection without holding the map lock afterward.
    fn projection(
        &self,
        session_id: &SessionId,
    ) -> Result<Arc<SessionProjection>, ProjectionError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_poison_error| ProjectionError::Poisoned)?;
        Ok(Arc::clone(
            sessions
                .entry(session_id.clone())
                .or_insert_with(|| Arc::new(SessionProjection::new(256))),
        ))
    }
}

/// Kernel event sink that journals and broadcasts ACP updates independently of transports.
struct ProjectionSink {
    session_id: SessionId,
    registry: Arc<ProjectionRegistry>,
}

#[async_trait]
impl EventSink for ProjectionSink {
    /// Maps one Kernel event once before retaining and broadcasting its ACP group.
    async fn emit(&self, event: AgentEvent) -> Result<(), SinkError> {
        let projection =
            self.registry
                .projection(&self.session_id)
                .map_err(|error| {
                    tracing::error!(
                        "failed to resolve ACP projection for Session {}: {}",
                        self.session_id,
                        error
                    );
                    SinkError::Consumer(error.to_string())
                })?;
        let active_operation =
            projection.active_operation().map_err(|error| {
                tracing::error!(
                    "failed to read active ACP Operation for Session {}: {}",
                    self.session_id,
                    error
                );
                SinkError::Consumer(error.to_string())
            })?;
        let (run_id, operation_phase, operation_kind) = match &event.payload {
            AgentEventPayload::RunStart { run_id } => (
                Some(run_id.clone()),
                Some(OperationPhase::Start),
                Some(OperationKind::Run),
            ),
            AgentEventPayload::SlashCommandStart { run_id, .. } => (
                Some(run_id.clone()),
                Some(OperationPhase::Start),
                Some(OperationKind::SlashCommand),
            ),
            AgentEventPayload::AgentSettled { run_id, .. } => (
                Some(run_id.clone()),
                Some(OperationPhase::End),
                Some(OperationKind::Run),
            ),
            AgentEventPayload::SlashCommandEnd { run_id, .. } => (
                Some(run_id.clone()),
                Some(OperationPhase::End),
                Some(OperationKind::SlashCommand),
            ),
            AgentEventPayload::CompactionStart { run_id, .. }
                if active_operation.is_none() =>
            {
                (
                    Some(run_id.clone()),
                    Some(OperationPhase::Start),
                    Some(OperationKind::Compaction),
                )
            }
            AgentEventPayload::CompactionEnd { run_id, .. }
                if active_operation.as_ref().is_some_and(
                    |(active_run_id, kind)| {
                        active_run_id == run_id
                            && *kind == OperationKind::Compaction
                    },
                ) =>
            {
                (
                    Some(run_id.clone()),
                    Some(OperationPhase::End),
                    Some(OperationKind::Compaction),
                )
            }
            AgentEventPayload::TurnStart { run_id }
            | AgentEventPayload::RetryScheduled { run_id, .. }
            | AgentEventPayload::RetryStart { run_id, .. }
            | AgentEventPayload::RetryEnd { run_id, .. }
            | AgentEventPayload::ToolExecutionStart { run_id, .. }
            | AgentEventPayload::ToolExecutionUpdate { run_id, .. }
            | AgentEventPayload::ToolExecutionEnd { run_id, .. }
            | AgentEventPayload::RunEnd { run_id, .. } => {
                (Some(run_id.clone()), None, None)
            }
            AgentEventPayload::CompactionStart { run_id, .. }
            | AgentEventPayload::CompactionEnd { run_id, .. } => {
                // A nested compaction has its own operation id, but recovery
                // groups must remain correlated with the active Agent Run.
                let recovery_run_id = active_operation.as_ref().map_or_else(
                    || run_id.clone(),
                    |(run_id, _kind)| run_id.clone(),
                );
                (Some(recovery_run_id), None, None)
            }
            _ => (active_operation.map(|(run_id, _kind)| run_id), None, None),
        };
        let baseline = if operation_phase == Some(OperationPhase::Start) {
            self.registry
                .kernel
                .session_replay(&self.session_id)
                .map_err(|error| {
                    tracing::error!(
                        "failed to capture ACP projection baseline for Session {}: {}",
                        self.session_id,
                        error
                    );
                    SinkError::Consumer(error.to_string())
                })?
        } else {
            Vec::new()
        };
        let metadata = AcpEventMapper::metadata(&event).map_err(|error| {
            tracing::error!(
                "failed to map ACP metadata for Session {}: {}",
                self.session_id,
                error
            );
            SinkError::Consumer(error.to_string())
        })?;
        let sequence = event.metadata.sequence;
        let updates = AcpEventMapper::map(event).map_err(|error| {
            tracing::error!(
                "failed to map ACP updates for Session {} at sequence {}: {}",
                self.session_id,
                sequence,
                error
            );
            SinkError::Consumer(error.to_string())
        })?;
        let builder = EventGroup::builder()
            .sequence(sequence)
            .metadata(metadata)
            .updates(updates);
        let group = match (run_id, operation_phase, operation_kind) {
            (Some(run_id), Some(operation_phase), Some(operation_kind)) => {
                builder
                    .run_id(run_id)
                    .operation_phase(operation_phase)
                    .operation_kind(operation_kind)
                    .build()
            }
            (Some(run_id), None, _) => builder.run_id(run_id).build(),
            (None, Some(_), _) => {
                tracing::error!(
                    "rejected ACP Operation boundary without Run id for Session {} at sequence {}",
                    self.session_id,
                    sequence
                );
                return Err(SinkError::Consumer(
                    "ACP Operation boundary has no Run id".to_string(),
                ));
            }
            (None, None, _) => builder.build(),
            (Some(_), Some(_), None) => {
                tracing::error!(
                    "rejected ACP Operation boundary without kind for Session {} at sequence {}",
                    self.session_id,
                    sequence
                );
                return Err(SinkError::Consumer(
                    "ACP Operation boundary has no Operation kind".to_string(),
                ));
            }
        };
        let group = Arc::new(group);
        projection
            .append_group(Arc::clone(&group), baseline)
            .map_err(|error| {
                tracing::error!(
                    "failed to append ACP projection for Session {} at sequence {}: {}",
                    self.session_id,
                    sequence,
                    error
                );
                SinkError::Consumer(error.to_string())
            })?;
        if let (Some(run_id), Some(operation_phase)) =
            (&group.run_id, group.operation_phase)
        {
            tracing::info!(
                "recorded ACP Operation {:?} for Session {}, Run {}, sequence {}",
                operation_phase,
                self.session_id,
                run_id,
                sequence
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use agent_client_protocol::schema::v2 as wire;
    use extension::StaticExtensionFactory;
    use futures::stream;
    use kernel::{
        Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
        NanoidIdGenerator,
    };
    use prompt::FilesystemPromptFactory;
    use protocol::{
        AgentEvent, AgentEventPayload, AgentOutcome, CompactionOutcome,
        CompactionReason, EventMetadata, ModelFinal, ModelProfile,
        ModelRequest, ModelStreamEvent, ModelUsage, RunId, Sequence, SessionId,
        StopReason, TimestampMs, TurnId,
    };
    use store::{JsonlStoreFactory, SessionCreateOptions, SystemClock};
    use tokio_util::sync::CancellationToken;
    use tools::BuiltinToolFactory;

    use super::{
        DurableResumeSelection, EventGroup, ProjectionError,
        ProjectionRegistry, SessionProjection,
    };
    use crate::recovery::{OperationPhase, RecoveryMode, SessionCursor};

    /// Deterministic local model used only to construct a real Kernel boundary.
    struct ProjectionModel {
        profile: ModelProfile,
    }

    #[async_trait::async_trait]
    impl Model for ProjectionModel {
        /// Returns the local projection-test model profile.
        fn profile(&self) -> &ModelProfile {
            &self.profile
        }

        /// Confirms that the local model has no external readiness dependency.
        async fn preflight(&self) -> Result<(), ModelError> {
            Ok(())
        }

        /// Returns one successful empty model turn.
        async fn stream(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> Result<ModelStream, ModelError> {
            Ok(Box::pin(stream::iter([Ok(ModelStreamEvent::Finished(
                ModelFinal {
                    stop_reason: StopReason::EndTurn,
                    raw_stop_reason: None,
                    usage: ModelUsage::builder()
                        .input_tokens(0)
                        .output_tokens(0)
                        .cache_read_tokens(0)
                        .cache_write_tokens(0)
                        .total_tokens(0)
                        .build(),
                },
            ))])))
        }
    }

    /// Returns the same in-memory model for each Kernel Session runtime.
    struct ProjectionModelFactory(Arc<ProjectionModel>);

    impl ModelFactory for ProjectionModelFactory {
        /// Clones the shared local model.
        fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
            Ok(Arc::clone(&self.0) as Arc<dyn Model>)
        }
    }

    /// Builds a production Kernel around local deterministic dependencies.
    fn projection_kernel(root: &Path) -> Arc<Kernel> {
        let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
        let model = Arc::new(ProjectionModel {
            profile: ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id("projection".to_string())
                .display_name("Projection fixture".to_string())
                .context_tokens(8_192)
                .max_output_tokens(1_024)
                .build(),
        });
        Arc::new(
            KernelFactory::builder()
                .model_factory(Arc::new(ProjectionModelFactory(model)))
                .tool_factory(Arc::new(BuiltinToolFactory::new()))
                .store_factory(Arc::new(JsonlStoreFactory::new(
                    root,
                    Arc::clone(&clock),
                )))
                .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                    root.join("config"),
                    protocol::PromptPolicy::default(),
                )))
                .extension_factory(Arc::new(StaticExtensionFactory::default()))
                .clock(clock)
                .id_generator(Arc::new(NanoidIdGenerator))
                .build()
                .build()
                .expect("projection Kernel"),
        )
    }

    /// Builds one empty projected group for journal boundary tests.
    fn group(
        run_id: &str,
        sequence: u64,
        operation_phase: Option<OperationPhase>,
    ) -> Arc<EventGroup> {
        let builder = EventGroup::builder()
            .run_id(RunId::try_from(run_id).expect("Run id"))
            .sequence(Sequence::try_from(sequence).expect("sequence"))
            .metadata(wire::Meta::default())
            .updates(Vec::new());
        match operation_phase {
            Some(operation_phase) => {
                Arc::new(builder.operation_phase(operation_phase).build())
            }
            None => Arc::new(builder.build()),
        }
    }

    /// One projected Kernel event is broadcast identically to every watcher.
    #[tokio::test]
    async fn broadcasts_one_group_to_multiple_watchers() {
        let projection = SessionProjection::new(8);
        let mut first = projection.subscribe();
        let mut second = projection.subscribe();
        let group = Arc::new(
            EventGroup::builder()
                .run_id(RunId::try_from("run-1").expect("Run id"))
                .sequence(Sequence::try_from(1_u64).expect("sequence"))
                .metadata(wire::Meta::default())
                .updates(vec![wire::SessionUpdate::StateUpdate(
                    wire::StateUpdate::Running(wire::RunningStateUpdate::new()),
                )])
                .operation_phase(OperationPhase::Start)
                .build(),
        );

        projection
            .append_group(Arc::clone(&group), Vec::new())
            .expect("append projected group");

        let first_group = first.recv().await.expect("first watcher group");
        let second_group = second.recv().await.expect("second watcher group");
        assert!(Arc::ptr_eq(&group, &first_group));
        assert!(Arc::ptr_eq(&group, &second_group));
    }

    /// Publishing without a watcher keeps the journal available for later recovery.
    #[test]
    fn appends_group_without_live_watchers() {
        let projection = SessionProjection::new(8);
        let group = Arc::new(
            EventGroup::builder()
                .run_id(RunId::try_from("run-1").expect("Run id"))
                .sequence(Sequence::try_from(1_u64).expect("sequence"))
                .metadata(wire::Meta::default())
                .updates(Vec::new())
                .operation_phase(OperationPhase::Start)
                .build(),
        );

        let run_id = RunId::try_from("run-1").expect("Run id");
        projection
            .append_group(group, Vec::new())
            .expect("append without watchers");

        let backlog = projection
            .snapshot_from(
                &run_id,
                Sequence::try_from(1_u64).expect("sequence"),
            )
            .expect("retained journal");
        assert_eq!(backlog.len(), 1);
    }

    /// A completed journal remains recoverable until the next Operation starts.
    #[test]
    fn retains_completed_journal_until_next_operation() {
        let projection = SessionProjection::new(8);
        let first_run = RunId::try_from("run-1").expect("first Run id");
        projection
            .append_group(
                group("run-1", 10, Some(OperationPhase::Start)),
                Vec::new(),
            )
            .expect("append first start");
        projection
            .append_group(
                group("run-1", 11, Some(OperationPhase::End)),
                Vec::new(),
            )
            .expect("append first end");

        let completed = projection
            .snapshot_from(
                &first_run,
                Sequence::try_from(11_u64).expect("completed sequence"),
            )
            .expect("completed journal");
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed
                .first()
                .expect("completed terminal group")
                .operation_phase,
            Some(OperationPhase::End)
        );

        projection
            .append_group(
                group("run-2", 12, Some(OperationPhase::Start)),
                Vec::new(),
            )
            .expect("append replacement start");
        let error = projection
            .snapshot_from(
                &first_run,
                Sequence::try_from(11_u64).expect("stale sequence"),
            )
            .expect_err("replacement must invalidate old Run cursor");
        assert_eq!(
            error,
            ProjectionError::CursorUnavailable,
            "replacement must reject the stale journal"
        );
    }

    /// Cursor validation rejects gaps while accepting exactly tail plus one.
    #[test]
    fn validates_incremental_cursor_range() {
        let projection = SessionProjection::new(8);
        let run_id = RunId::try_from("run-1").expect("Run id");
        projection
            .append_group(
                group("run-1", 10, Some(OperationPhase::Start)),
                Vec::new(),
            )
            .expect("append start");
        projection
            .append_group(group("run-1", 11, None), Vec::new())
            .expect("append update");

        let early_error = projection
            .snapshot_from(
                &run_id,
                Sequence::try_from(9_u64).expect("early sequence"),
            )
            .expect_err("cursor before journal head must fail");
        assert_eq!(
            early_error,
            ProjectionError::CursorUnavailable,
            "cursor before the journal head must be unavailable"
        );
        assert!(
            projection
                .snapshot_from(
                    &run_id,
                    Sequence::try_from(12_u64).expect("tail sequence"),
                )
                .expect("tail plus one cursor")
                .is_empty()
        );
        let late_error = projection
            .snapshot_from(
                &run_id,
                Sequence::try_from(13_u64).expect("late sequence"),
            )
            .expect_err("cursor after tail plus one must fail");
        assert_eq!(
            late_error,
            ProjectionError::CursorUnavailable,
            "cursor after tail plus one must be unavailable"
        );
    }

    /// Subscription snapshots backlog before continuing with later live groups.
    #[tokio::test]
    async fn subscribes_across_backlog_and_live_boundary() {
        let projection = SessionProjection::new(8);
        let run_id = RunId::try_from("run-1").expect("Run id");
        projection
            .append_group(
                group("run-1", 10, Some(OperationPhase::Start)),
                Vec::new(),
            )
            .expect("append start");
        projection
            .append_group(group("run-1", 11, None), Vec::new())
            .expect("append backlog update");

        let mut subscription = projection
            .subscribe_from_cursor(
                &run_id,
                Sequence::try_from(11_u64).expect("cursor sequence"),
            )
            .expect("subscribe from cursor");
        projection
            .append_group(group("run-1", 12, None), Vec::new())
            .expect("append live update");

        assert_eq!(subscription.backlog.len(), 1);
        assert_eq!(subscription.tail_sequence.get(), 11);
        let live = subscription.receiver.recv().await.expect("live group");
        assert_eq!(live.sequence.get(), 12);
    }

    /// Full projection recovery returns the whole retained Operation journal.
    #[test]
    fn subscribes_from_operation_start() {
        let projection = SessionProjection::new(8);
        projection
            .append_group(
                group("run-1", 10, Some(OperationPhase::Start)),
                Vec::new(),
            )
            .expect("append start");
        projection
            .append_group(
                group("run-1", 11, Some(OperationPhase::End)),
                Vec::new(),
            )
            .expect("append end");

        let subscription = projection
            .subscribe_from_start()
            .expect("subscribe from Operation start");

        assert_eq!(subscription.backlog.len(), 2);
        assert_eq!(subscription.next_sequence.get(), 10);
        assert_eq!(subscription.tail_sequence.get(), 11);
        assert!(!subscription.running);
    }

    /// The shared sink maps a Kernel event once into the Session projection stream.
    #[tokio::test]
    async fn projection_sink_maps_and_journals_kernel_event() {
        let state = tempfile::tempdir().expect("store root");
        let workspace = tempfile::tempdir().expect("workspace root");
        let kernel = projection_kernel(state.path());
        let session_id =
            SessionId::try_from("session-projection").expect("Session id");
        kernel
            .create_session(SessionCreateOptions {
                session_id: session_id.clone(),
                cwd: workspace.path().to_path_buf(),
                parent_session_id: None,
            })
            .await
            .expect("create Session");
        let registry = Arc::new(ProjectionRegistry::new(kernel));
        let mut receiver = registry
            .subscribe_live(&session_id)
            .expect("subscribe live projection");
        let sink = registry.sink(session_id.clone());

        sink.emit(AgentEvent {
            metadata: EventMetadata {
                turn_id: TurnId::try_from("turn-1").expect("Turn id"),
                timestamp_ms: TimestampMs::from(1_u64),
                sequence: Sequence::try_from(1_u64).expect("sequence"),
            },
            payload: AgentEventPayload::RunStart {
                run_id: RunId::try_from("run-1").expect("Run id"),
            },
        })
        .await
        .expect("emit projected event");

        let group = receiver.recv().await.expect("projected group");
        assert_eq!(group.run_id.as_ref().map(RunId::as_str), Some("run-1"));
        assert_eq!(group.operation_phase, Some(OperationPhase::Start));
        assert!(!group.updates.is_empty());
    }

    /// Nested compaction events stay inside the active Agent Run recovery journal.
    #[tokio::test]
    async fn nested_compaction_uses_active_agent_run_id() {
        let state = tempfile::tempdir().expect("store root");
        let workspace = tempfile::tempdir().expect("workspace root");
        let kernel = projection_kernel(state.path());
        let session_id =
            SessionId::try_from("session-compaction").expect("Session id");
        kernel
            .create_session(SessionCreateOptions {
                session_id: session_id.clone(),
                cwd: workspace.path().to_path_buf(),
                parent_session_id: None,
            })
            .await
            .expect("create Session");
        let registry = Arc::new(ProjectionRegistry::new(kernel));
        let sink = registry.sink(session_id.clone());
        let agent_run = RunId::try_from("run-agent").expect("Agent Run id");
        let compaction_run =
            RunId::try_from("run-compaction").expect("compaction Run id");
        let payloads = [
            AgentEventPayload::RunStart {
                run_id: agent_run.clone(),
            },
            AgentEventPayload::CompactionStart {
                run_id: compaction_run.clone(),
                reason: CompactionReason::Manual,
            },
            AgentEventPayload::CompactionEnd {
                run_id: compaction_run,
                reason: CompactionReason::Manual,
                outcome: CompactionOutcome::Cancelled,
            },
            AgentEventPayload::AgentSettled {
                run_id: agent_run.clone(),
                outcome: AgentOutcome::Succeeded,
            },
        ];
        for (index, payload) in payloads.into_iter().enumerate() {
            sink.emit(AgentEvent {
                metadata: EventMetadata {
                    turn_id: TurnId::try_from("turn-compaction")
                        .expect("Turn id"),
                    timestamp_ms: TimestampMs::from(index as u64 + 1),
                    sequence: Sequence::try_from(index as u64 + 1)
                        .expect("sequence"),
                },
                payload,
            })
            .await
            .expect("emit projected event");
        }

        let subscription = registry
            .subscribe_from_start(&session_id)
            .expect("retained Agent Run projection");

        assert_eq!(subscription.backlog.len(), 4);
        assert!(
            subscription
                .backlog
                .iter()
                .all(|group| { group.run_id.as_ref() == Some(&agent_run) })
        );
        assert_eq!(
            subscription
                .backlog
                .iter()
                .map(|group| group.operation_phase)
                .collect::<Vec<_>>(),
            vec![
                Some(OperationPhase::Start),
                None,
                None,
                Some(OperationPhase::End),
            ]
        );
        assert!(!subscription.running);
    }

    /// A journal appearing after durable capture replaces that snapshot completely.
    #[tokio::test]
    async fn durable_resume_selects_new_journal_without_mixing_replay() {
        let state = tempfile::tempdir().expect("store root");
        let workspace = tempfile::tempdir().expect("workspace root");
        let kernel = projection_kernel(state.path());
        let session_id =
            SessionId::try_from("session-durable-race").expect("Session id");
        kernel
            .create_session(SessionCreateOptions {
                session_id: session_id.clone(),
                cwd: workspace.path().to_path_buf(),
                parent_session_id: None,
            })
            .await
            .expect("create Session");
        let registry = Arc::new(ProjectionRegistry::new(kernel));
        let live_receiver = registry
            .subscribe_live(&session_id)
            .expect("pre-resume live receiver");
        let sink = registry.sink(session_id.clone());
        sink.emit(AgentEvent {
            metadata: EventMetadata {
                turn_id: TurnId::try_from("turn-durable-race")
                    .expect("Turn id"),
                timestamp_ms: TimestampMs::from(1_u64),
                sequence: Sequence::try_from(1_u64).expect("sequence"),
            },
            payload: AgentEventPayload::RunStart {
                run_id: RunId::try_from("run-durable-race").expect("Run id"),
            },
        })
        .await
        .expect("start concurrent Run");

        let selection = registry
            .select_durable_resume(&session_id, live_receiver, Vec::new())
            .expect("select durable resume source");

        let DurableResumeSelection::Retained(subscription) = selection else {
            panic!("new journal must replace the captured durable replay");
        };
        assert_eq!(subscription.backlog.len(), 1);
        assert_eq!(subscription.run_id.as_str(), "run-durable-race");
    }

    /// Every notification in one mapped event carries a complete atomic group boundary.
    #[test]
    fn projected_group_adds_recovery_metadata_to_notifications() {
        let group = EventGroup::builder()
            .run_id(RunId::try_from("run-1").expect("Run id"))
            .sequence(Sequence::try_from(7_u64).expect("sequence"))
            .metadata(wire::Meta::default())
            .updates(vec![
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(
                    wire::RunningStateUpdate::new(),
                )),
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(
                    wire::IdleStateUpdate::new(),
                )),
            ])
            .operation_phase(OperationPhase::Start)
            .build();

        let notifications = group
            .notifications(
                &SessionId::try_from("session-1").expect("Session id"),
                "clawcode",
            )
            .expect("build grouped notifications");

        assert_eq!(notifications.len(), 2);
        for (index, notification) in notifications.iter().enumerate() {
            let value = serde_json::to_value(notification)
                .expect("serialize grouped notification");
            assert_eq!(
                value["_meta"]["clawcode"]["sessionRecovery"],
                serde_json::json!({
                    "runId": "run-1",
                    "projectionIndex": index,
                    "projectionCount": 2,
                    "operationPhase": "start"
                })
            );
        }
    }

    /// Recovery planning preserves a cursor only while its journal is covered.
    #[test]
    fn plans_watch_for_covered_cursor_and_resume_for_stale_run() {
        let projection = SessionProjection::new(8);
        projection
            .append_group(
                group("run-1", 10, Some(OperationPhase::Start)),
                Vec::new(),
            )
            .expect("append start");
        let session_id = SessionId::try_from("session-1").expect("Session id");
        let watch = projection
            .recovery_plan(&SessionCursor {
                session_id: session_id.clone(),
                run_id: RunId::try_from("run-1").expect("Run id"),
                next_sequence: Sequence::try_from(11_u64)
                    .expect("next sequence"),
            })
            .expect("watch plan");
        let resume = projection
            .recovery_plan(&SessionCursor {
                session_id,
                run_id: RunId::try_from("run-old").expect("stale Run id"),
                next_sequence: Sequence::try_from(10_u64)
                    .expect("stale sequence"),
            })
            .expect("Resume plan");

        assert_eq!(watch.mode, RecoveryMode::Watch);
        assert_eq!(resume.mode, RecoveryMode::Resume);
    }

    /// Fatal Kernel errors publish Idle while forcing the next attachment to replay fully.
    #[tokio::test]
    async fn terminal_fallback_invalidates_incremental_journal() {
        let projection = SessionProjection::new(8);
        let mut receiver = projection.subscribe();
        projection
            .append_group(
                group("run-1", 10, Some(OperationPhase::Start)),
                Vec::new(),
            )
            .expect("append start");
        receiver.recv().await.expect("start broadcast");

        projection
            .publish_terminal_fallback()
            .expect("publish terminal fallback");

        let terminal = receiver.recv().await.expect("terminal broadcast");
        assert!(terminal.run_id.is_none());
        assert!(matches!(
            terminal.updates.as_slice(),
            [wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(_))]
        ));
        let plan = projection
            .recovery_plan(&SessionCursor {
                session_id: SessionId::try_from("session-1")
                    .expect("Session id"),
                run_id: RunId::try_from("run-1").expect("Run id"),
                next_sequence: Sequence::try_from(11_u64)
                    .expect("next sequence"),
            })
            .expect("fallback plan");
        assert_eq!(plan.mode, RecoveryMode::Resume);
    }
}
