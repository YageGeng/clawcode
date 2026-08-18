use std::collections::VecDeque;
use std::sync::Arc;

use protocol::{
    AgentMessage, EntryId, IdKind, MessageIdentity, MessageTiming,
    PendingMessages, QueueId, QueueKind, QueuedMessage, RecordId, SessionId,
    TurnId,
};
use serde::{Deserialize, Serialize};
use store::{NewRecord, RecordKind, SessionRecord};

use super::input::PromptInputExpansion;
use super::{EventEmitter, Kernel, KernelError, SessionRuntime};

impl Kernel {
    /// Persists and queues a complete user message for the active run.
    pub async fn queue_message(
        &self,
        session_id: &SessionId,
        kind: QueueKind,
        input: impl Into<protocol::RunInput>,
    ) -> Result<QueuedMessage, KernelError> {
        let input = input.into();
        let session = self.session(session_id)?;
        let run_id = session
            .active_run_id
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .clone()
            .ok_or_else(|| {
                KernelError::SessionNotRunning(session_id.clone())
            })?;
        if let Some(command_input) = input.slash_command_text()
            && let Ok(invocation) =
                protocol::SlashCommandInvocation::try_from(command_input)
        {
            match session.resolve_direct_slash_command(&invocation)? {
                super::command::DirectSlashCommandResolution::Command(
                    command,
                ) => {
                    return Err(KernelError::SlashCommandCannotQueue {
                        name: invocation.name,
                        command_source: command.source(),
                    });
                }
                super::command::DirectSlashCommandResolution::Rejected {
                    error,
                    ..
                } => return Err(KernelError::SlashCommandRejected(error)),
                super::command::DirectSlashCommandResolution::NotFound => {}
            }
        }
        let expanded = match input {
            protocol::RunInput::Text(text) => {
                session.expand_prompt_input(session_id, &text)?
            }
            protocol::RunInput::Blocks(blocks) => {
                PromptInputExpansion::from_blocks(blocks)
            }
        };
        if let Some(diagnostic) = expanded.diagnostic().cloned() {
            let sink = session
                .event_sink
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)?
                .clone()
                .ok_or_else(|| {
                    KernelError::Protocol(
                        "active run is missing its event sink".to_string(),
                    )
                })?;
            let turn_id = session
                .diagnostic_turn_id
                .lock()
                .map_err(|_poison_error| KernelError::Poisoned)?
                .clone()
                .ok_or_else(|| {
                    KernelError::Protocol(
                        "active run is missing its diagnostic TurnId"
                            .to_string(),
                    )
                })?;
            EventEmitter {
                clock: Arc::clone(&self.clock),
                sink,
                session: Arc::clone(&session),
            }
            .emit(
                turn_id,
                protocol::AgentEventPayload::SkillDiagnostic { diagnostic },
            )
            .await?;
        }
        let content = expanded.message_content();
        let timestamp = self.clock.now();
        let queued = QueuedMessage::builder()
            .queue_id(
                QueueId::try_from(self.id_generator.next(IdKind::Queue))
                    .map_err(|error| {
                        KernelError::Protocol(error.to_string())
                    })?,
            )
            .kind(kind)
            .message(AgentMessage {
                identity: MessageIdentity {
                    message_id: self.message_id()?,
                    turn_id: TurnId::try_from(
                        self.id_generator.next(IdKind::Turn),
                    )
                    .map_err(|error| {
                        KernelError::Protocol(error.to_string())
                    })?,
                },
                timing: MessageTiming::try_from((
                    timestamp, timestamp, timestamp,
                ))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
                content,
            })
            .build();
        let item = PendingQueueItem::new(
            queued.clone(),
            EntryId::try_from(self.id_generator.next(IdKind::Entry))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
        );
        self.record_operation(
            &session,
            &run_id,
            RecordKind::QueueEnqueued,
            item.enqueued_payload()?,
        )?;
        // Queue acceptance is externally acknowledged by this return value and
        // may race the active Run's final checkpoint.
        session.sync_store()?;
        let mut queue = session
            .queue
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        queue.push(item);
        Ok(queued)
    }

    /// Returns a defensive snapshot of complete queued messages.
    pub fn pending_messages(
        &self,
        session_id: &SessionId,
    ) -> Result<PendingMessages, KernelError> {
        let session = self.session(session_id)?;
        let queue = session
            .queue
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        Ok(queue.snapshot())
    }

    /// Cancels one queued message durably before removing its in-memory view.
    pub fn remove_pending_message(
        &self,
        session_id: &SessionId,
        queue_id: &QueueId,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        let exists = session
            .queue
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .ids()
            .iter()
            .any(|candidate| candidate == queue_id);
        if !exists {
            return Err(KernelError::PendingMessageNotFound(queue_id.clone()));
        }
        self.record_queue_cancellation(
            &session,
            None,
            queue_id.clone(),
            QueueCancellationReason::Removed,
        )?;
        session.sync_store()?;
        session
            .queue
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .remove(queue_id);
        Ok(())
    }

    /// Clears all queued messages after persisting one cancellation per item.
    pub fn clear_pending_messages(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        let queue_ids = session
            .queue
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .ids();
        for queue_id in &queue_ids {
            self.record_queue_cancellation(
                &session,
                None,
                queue_id.clone(),
                QueueCancellationReason::Cleared,
            )?;
        }
        session.sync_store()?;
        let mut queue = session
            .queue
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        for queue_id in queue_ids {
            queue.remove(&queue_id);
        }
        Ok(())
    }

    /// Appends a queue cancellation with an optional active-run correlation.
    pub(super) fn record_queue_cancellation(
        &self,
        session: &SessionRuntime,
        run_id: Option<&protocol::RunId>,
        queue_id: QueueId,
        reason: QueueCancellationReason,
    ) -> Result<(), KernelError> {
        let record_id =
            RecordId::try_from(self.id_generator.next(IdKind::Record))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .append_record(
                NewRecord::builder()
                    .id(record_id)
                    .lane(session.lane.clone())
                    .run_id(run_id.cloned())
                    .kind(RecordKind::QueueCancelled)
                    .payload(cancelled_payload(queue_id, reason)?)
                    .build(),
            )?;
        Ok(())
    }
}

/// One queued message plus the entry identifier reserved before persistence.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PendingQueueItem {
    pub(super) queued: QueuedMessage,
    pub(super) entry_id: EntryId,
}

impl PendingQueueItem {
    /// Creates an in-memory queue item after all public identifiers are validated.
    pub(super) fn new(queued: QueuedMessage, entry_id: EntryId) -> Self {
        Self { queued, entry_id }
    }

    /// Encodes the redundant pi queue fields used for recovery validation.
    pub(super) fn enqueued_payload(
        &self,
    ) -> Result<serde_json::Value, KernelError> {
        serde_json::to_value(
            QueueEnqueuedData::builder()
                .queue(self.queued.clone())
                .queue_id(self.queued.queue_id.clone())
                .target(self.queued.kind)
                .entry_id(self.entry_id.clone())
                .build(),
        )
        .map_err(KernelError::from)
    }
}

/// Reason a durable queue record no longer participates in recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum QueueCancellationReason {
    /// The queued message was persisted into a Turn.
    Consumed,
    /// A client removed one queue item explicitly.
    Removed,
    /// A client cleared every pending queue item.
    Cleared,
}

/// In-memory view reconstructed from append-only queue records.
#[derive(Debug, Default)]
pub(super) struct PendingQueue {
    steering: VecDeque<PendingQueueItem>,
    follow_up: VecDeque<PendingQueueItem>,
}

impl PendingQueue {
    /// Replays queue records and retains only items without a later cancellation.
    pub(super) fn from_records(
        records: &[SessionRecord],
    ) -> Result<Self, KernelError> {
        let mut queue = Self::default();
        for record in records {
            let payload = serde_json::Value::Object(record.payload.clone());
            match record.kind {
                RecordKind::QueueEnqueued => {
                    let data: QueueEnqueuedData =
                        serde_json::from_value(payload)?;
                    if data.queue.queue_id != data.queue_id
                        || data.queue.kind != data.target
                    {
                        return Err(KernelError::Protocol(
                            "queue record correlation fields disagree"
                                .to_string(),
                        ));
                    }
                    queue
                        .push(PendingQueueItem::new(data.queue, data.entry_id));
                }
                RecordKind::QueueCancelled => {
                    let data: QueueCancelledData =
                        serde_json::from_value(payload)?;
                    queue.remove(&data.queue_id);
                }
                RecordKind::OperationStarted
                | RecordKind::AbortRequested
                | RecordKind::OperationFinished
                | RecordKind::StepAttempt
                | RecordKind::ToolStarted
                | RecordKind::WriteDeferred
                | RecordKind::Usage => {}
            }
        }
        Ok(queue)
    }

    /// Adds one item to its typed scheduling lane.
    pub(super) fn push(&mut self, item: PendingQueueItem) {
        match item.queued.kind {
            QueueKind::Steering => self.steering.push_back(item),
            QueueKind::FollowUp => self.follow_up.push_back(item),
        }
    }

    /// Returns the next eligible item without removing it before persistence.
    pub(super) fn next(
        &self,
        allow_follow_up: bool,
    ) -> Option<PendingQueueItem> {
        self.steering.front().cloned().or_else(|| {
            allow_follow_up
                .then(|| self.follow_up.front().cloned())
                .flatten()
        })
    }

    /// Removes one item by stable identifier after its cancellation is durable.
    pub(super) fn remove(
        &mut self,
        queue_id: &QueueId,
    ) -> Option<PendingQueueItem> {
        if let Some(index) = self
            .steering
            .iter()
            .position(|item| &item.queued.queue_id == queue_id)
        {
            return self.steering.remove(index);
        }
        self.follow_up
            .iter()
            .position(|item| &item.queued.queue_id == queue_id)
            .and_then(|index| self.follow_up.remove(index))
    }

    /// Returns every pending queue identifier in deterministic scheduling order.
    pub(super) fn ids(&self) -> Vec<QueueId> {
        self.steering
            .iter()
            .chain(self.follow_up.iter())
            .map(|item| item.queued.queue_id.clone())
            .collect()
    }

    /// Clones the public queue view without exposing reserved entry identifiers.
    pub(super) fn snapshot(&self) -> PendingMessages {
        PendingMessages::builder()
            .steering(
                self.steering
                    .iter()
                    .map(|item| item.queued.clone())
                    .collect(),
            )
            .follow_up(
                self.follow_up
                    .iter()
                    .map(|item| item.queued.clone())
                    .collect(),
            )
            .build()
    }
}

/// Builds a typed cancellation payload for durable queue mutation records.
fn cancelled_payload(
    queue_id: QueueId,
    reason: QueueCancellationReason,
) -> Result<serde_json::Value, KernelError> {
    serde_json::to_value(QueueCancelledData { queue_id, reason })
        .map_err(KernelError::from)
}

#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
#[serde(rename_all = "camelCase")]
struct QueueEnqueuedData {
    queue: QueuedMessage,
    queue_id: QueueId,
    target: QueueKind,
    entry_id: EntryId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueCancelledData {
    queue_id: QueueId,
    reason: QueueCancellationReason,
}
