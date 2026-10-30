use std::collections::VecDeque;
use std::sync::Arc;

use protocol::{
    AgentMessage, EntryId, IdKind, MessageIdentity, MessageTiming,
    PendingMessages, QueueId, QueueKind, QueuedMessage, RecordId, SessionId,
    TurnId,
};
use serde::{Deserialize, Serialize};
use store::{NewRecord, RecordKind, SessionRecord, SessionStore};

use super::input::PromptInputExpansion;
use super::{
    EventEmitter, InsertionNotice, Kernel, KernelError, Session,
    SessionExecution,
};

impl Kernel {
    /// Persists and queues a complete user message for the active run.
    pub async fn queue_message(
        &self,
        session_id: &SessionId,
        kind: QueueKind,
        input: impl Into<protocol::RunInput>,
    ) -> Result<QueuedMessage, KernelError> {
        self.enqueue(
            session_id,
            QueueRequest::builder()
                .kind(kind)
                .input(input.into())
                .build(),
        )
        .await
    }

    /// Accepts input only while an agent Run can consume the durable queue.
    pub(super) async fn enqueue(
        &self,
        session_id: &SessionId,
        request: QueueRequest,
    ) -> Result<QueuedMessage, KernelError> {
        let QueueRequest {
            kind,
            input,
            waiter,
            submission_id,
            input_processed,
            cancellation,
        } = request;
        tracing::debug!(
            "started Queue enqueue {:?} for session {}",
            kind,
            session_id
        );
        let result: Result<(QueuedMessage, protocol::RunId), KernelError> =
            async {
                let session = self.session(session_id)?;
                session.transcript.ensure_writable()?;
                let run_id = session
                    .execution
                    .active_run()?
                    .map(|active_run| active_run.run_id)
                    .ok_or_else(|| {
                        KernelError::SessionNotRunning(session_id.clone())
                    })?;
                if !input_processed
                    && let Some(command_input) = input.slash_command_text()
                    && let Ok(invocation) =
                        protocol::SlashCommandInvocation::try_from(
                            command_input,
                        )
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
                    // Expansion performs synchronous resource I/O, so refresh the
                    // active Turn before correlating a diagnostic emitted afterward.
                    let active_run = session
                        .execution
                        .active_run()?
                        .filter(|active_run| active_run.run_id == run_id)
                        .ok_or_else(|| {
                            KernelError::SessionNotRunning(session_id.clone())
                        })?;
                    let sink = Arc::clone(&active_run.sink);
                    let turn_id = active_run.turn_id.clone();
                    EventEmitter {
                        clock: Arc::clone(&self.clock),
                        sink,
                        session: Arc::clone(&session),
                    }
                    .emit(
                        turn_id,
                        protocol::AgentEventPayload::SkillDiagnostic {
                            diagnostic,
                        },
                    )
                    .await?;
                }
                let content = expanded.message_content();
                let timestamp = self.clock.now();
                let queued = QueuedMessage::builder()
                    .queue_id(
                        QueueId::try_from(
                            self.id_generator.next(IdKind::Queue),
                        )
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?,
                    )
                    .kind(kind)
                    .submission_id(submission_id)
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
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?,
                        content,
                    })
                    .build();
                let mut item = PendingQueueItem::new(
                    queued.clone(),
                    EntryId::try_from(self.id_generator.next(IdKind::Entry))
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?,
                );
                item.waiter = waiter;
                let record_id =
                    RecordId::try_from(self.id_generator.next(IdKind::Record))
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?;
                session.enqueue_pending(
                    NewRecord::builder()
                        .id(record_id)
                        .lane(session.transcript.lane())
                        .run_id(Some(run_id.clone()))
                        .kind(RecordKind::QueueEnqueued)
                        .payload(item.enqueued_payload()?)
                        .build(),
                    item,
                    cancellation.as_ref(),
                )?;
                Ok((queued, run_id))
            }
            .await;
        match result {
            Ok((queued, run_id)) => {
                self.publish_queue_changed(session_id);
                tracing::info!(
                    "completed Queue enqueue {} as {:?} for session {} and Kernel Run {}, submission {:?}",
                    queued.queue_id,
                    queued.kind,
                    session_id,
                    run_id,
                    queued.submission_id
                );
                Ok(queued)
            }
            Err(
                error @ (KernelError::SessionNotRunning(_)
                | KernelError::PromptCancelled),
            ) => {
                tracing::debug!(
                    "queue admission ended for session {}: {}",
                    session_id,
                    error
                );
                Err(error)
            }
            Err(error) => {
                tracing::error!(
                    "failed Queue enqueue {:?} for session {}: {}",
                    kind,
                    session_id,
                    error
                );
                Err(error)
            }
        }
    }

    /// Returns a defensive snapshot of complete queued messages.
    pub fn pending_messages(
        &self,
        session_id: &SessionId,
    ) -> Result<PendingMessages, KernelError> {
        let session = self.session(session_id)?;
        session.execution.pending_snapshot()
    }

    /// Cancels one queued message durably before removing its in-memory view.
    pub fn remove_pending_message(
        &self,
        session_id: &SessionId,
        queue_id: &QueueId,
    ) -> Result<(), KernelError> {
        tracing::debug!(
            "started queued message removal {} for session {}",
            queue_id,
            session_id
        );
        let session = self.session(session_id)?;
        let exists = session
            .execution
            .pending_ids()?
            .iter()
            .any(|candidate| candidate == queue_id);
        if !exists {
            return Ok(());
        }
        let record = self.queue_cancellation_record(
            &session,
            None,
            queue_id.clone(),
            QueueCancellationReason::Removed,
        )?;
        session.remove_pending(record, queue_id)?;
        self.publish_queue_changed(session_id);
        tracing::info!(
            "completed queued message removal {} for session {}",
            queue_id,
            session_id
        );
        Ok(())
    }

    /// Clears all queued messages after persisting one cancellation per item.
    pub fn clear_pending_messages(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        tracing::debug!(
            "started queued message clear for session {}",
            session_id
        );
        let session = self.session(session_id)?;
        // Keep selection, durable cancellation, and draining under one lock so claims cannot race.
        let mut queue = session
            .execution
            .queue
            .lock()
            .map_err(|_error| KernelError::Poisoned)?;
        let queue_ids = queue.ids();
        let records = queue_ids
            .iter()
            .map(|queue_id| {
                self.queue_cancellation_record(
                    &session,
                    None,
                    queue_id.clone(),
                    QueueCancellationReason::Cleared,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        session.transcript.commit_records(records)?;
        for item in queue.steering.drain(..) {
            item.reject(&KernelError::PromptCancelled);
        }
        for item in queue.follow_up.drain(..) {
            item.reject(&KernelError::PromptCancelled);
        }
        drop(queue);
        self.publish_queue_changed(session_id);
        tracing::info!(
            "completed queued message clear of {} items for session {}",
            queue_ids.len(),
            session_id
        );
        Ok(())
    }

    /// Moves one pending follow-up to steering with a single replayable enqueue record.
    pub fn promote_pending_message(
        &self,
        session_id: &SessionId,
        queue_id: &QueueId,
    ) -> Result<bool, KernelError> {
        let session = self.session(session_id)?;
        let mut queue = session
            .execution
            .queue
            .lock()
            .map_err(|_error| KernelError::Poisoned)?;
        if queue
            .steering
            .iter()
            .any(|item| &item.queued.queue_id == queue_id)
        {
            return Ok(true);
        }
        let Some(item) = queue
            .follow_up
            .iter()
            .find(|item| &item.queued.queue_id == queue_id)
        else {
            return Ok(false);
        };
        let mut promoted = item.clone();
        promoted.queued.kind = QueueKind::Steering;
        let record = NewRecord::builder()
            .id(RecordId::try_from(self.id_generator.next(IdKind::Record))
                .map_err(|e| KernelError::Protocol(e.to_string()))?)
            .lane(session.transcript.lane())
            .kind(RecordKind::QueueEnqueued)
            .payload(promoted.enqueued_payload()?)
            .build();
        session.transcript.commit_records(vec![record])?;
        queue.remove(queue_id);
        queue.push(promoted);
        drop(queue);
        tracing::info!(
            "promoted queue item {} to steering for session {}",
            queue_id,
            session_id
        );
        self.publish_queue_changed(session_id);
        Ok(true)
    }

    /// Registers snapshot delivery so shutdown can wait for the final queue view.
    pub(super) fn publish_queue_changed(&self, session_id: &SessionId) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let session = match self.session(session_id) {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(
                    "failed scheduling queue snapshot for session {}: {}",
                    session_id,
                    error
                );
                return;
            }
        };
        let kernel = self.clone();
        let session_id = session_id.clone();
        let publishing = Arc::clone(&session);
        session.queue_publishes.spawn_on(
            async move {
                // Read and send under one asynchronous lock; late tasks cannot publish stale snapshots.
                let _guard = publishing.queue_publish.lock().await;
                let result: Result<(), KernelError> = async {
                    let pending = publishing.execution.pending_snapshot()?;
                    let active = publishing.execution.active_run()?;
                    let turn_id = match &active {
                        Some(active) => active.turn_id.clone(),
                        None => TurnId::try_from(
                            kernel.id_generator.next(IdKind::Turn),
                        )
                        .map_err(|e| KernelError::Protocol(e.to_string()))?,
                    };
                    let sink = active.map_or_else(
                        || kernel.session_sink(&session_id),
                        |active| active.sink,
                    );
                    EventEmitter {
                        clock: Arc::clone(&kernel.clock),
                        sink,
                        session: Arc::clone(&publishing),
                    }
                    .emit(
                        turn_id,
                        protocol::AgentEventPayload::QueueChanged { pending },
                    )
                    .await
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(
                        "failed publishing queue snapshot for session {}: {}",
                        session_id,
                        error
                    );
                }
            },
            &handle,
        );
    }

    /// Withdraws only live client waiters and delegated receipts after consumption has stopped.
    pub(super) fn withdraw_pending(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        let mut queue = session
            .execution
            .queue
            .lock()
            .map_err(|_error| KernelError::Poisoned)?;
        queue.open = false;
        let ids = queue
            .steering
            .iter()
            .chain(&queue.follow_up)
            .filter(|item| match &item.waiter {
                Some(QueueWaiter::Insertion(notice)) => notice.is_listening(),
                Some(QueueWaiter::Receipt(_)) => true,
                None => false,
            })
            .map(|item| item.queued.queue_id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(());
        }
        let records = ids
            .iter()
            .map(|id| {
                self.queue_cancellation_record(
                    &session,
                    None,
                    id.clone(),
                    QueueCancellationReason::Withdrawn,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        match session.transcript.commit_records(records) {
            Ok(()) => {
                for id in &ids {
                    if let Some(item) = queue.remove(id) {
                        item.reject(&KernelError::PromptCancelled);
                    }
                }
                drop(queue);
                self.publish_queue_changed(session_id);
                tracing::info!(
                    "withdrew {} queued inputs for session {}",
                    ids.len(),
                    session_id
                );
                Ok(())
            }
            Err(error) => {
                // An uncertain write cannot authorize returning input for resubmission.
                for item in queue.steering.iter().chain(&queue.follow_up) {
                    if ids.contains(&item.queued.queue_id) {
                        item.reject(&error);
                    }
                }
                Err(error)
            }
        }
    }

    /// Appends a queue cancellation with an optional active-run correlation.
    pub(super) fn queue_cancellation_record(
        &self,
        session: &Session,
        run_id: Option<&protocol::RunId>,
        queue_id: QueueId,
        reason: QueueCancellationReason,
    ) -> Result<NewRecord, KernelError> {
        let record_id =
            RecordId::try_from(self.id_generator.next(IdKind::Record))
                .map_err(|error| KernelError::Protocol(error.to_string()))?;
        Ok(NewRecord::builder()
            .id(record_id)
            .lane(session.transcript.lane())
            .run_id(run_id.cloned())
            .kind(RecordKind::QueueCancelled)
            .payload(cancelled_payload(queue_id, reason)?)
            .build())
    }
}

impl Session {
    /// Serializes acceptance with the Run's final queue check and durable commit.
    fn enqueue_pending(
        &self,
        record: NewRecord,
        item: PendingQueueItem,
        cancellation: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(), KernelError> {
        let mut queue = self
            .execution
            .queue
            .lock()
            .map_err(|_error| KernelError::Poisoned)?;
        if cancellation
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
        {
            return Err(KernelError::PromptCancelled);
        }
        if !queue.open {
            return Err(KernelError::SessionNotRunning(
                self.transcript.session_id()?,
            ));
        }
        self.transcript.commit_records(vec![record])?;
        queue.push(item);
        Ok(())
    }

    /// Commits removal under the same lock used by claims, so inserted items cannot be removed.
    pub(super) fn remove_pending(
        &self,
        record: NewRecord,
        queue_id: &QueueId,
    ) -> Result<(), KernelError> {
        let mut queue = self
            .execution
            .queue
            .lock()
            .map_err(|_error| KernelError::Poisoned)?;
        if !queue.ids().contains(queue_id) {
            return Ok(());
        }
        self.transcript.commit_records(vec![record])?;
        if let Some(item) = queue.remove(queue_id) {
            item.reject(&KernelError::PromptCancelled);
        }
        Ok(())
    }

    /// Ends the claim before the first write; uncertain commits must never be retried in memory.
    pub(super) fn consume_claimed(
        &self,
        mut claimed: ClaimedItem,
        message: &AgentMessage,
        record: NewRecord,
    ) -> Result<(), KernelError> {
        // Cancel and the first write share this boundary, including multi-threaded callers.
        let cancellation = self
            .execution
            .cancellation
            .lock()
            .map_err(|_error| KernelError::Poisoned)?;
        if cancellation.is_cancelled() {
            return Err(KernelError::PromptCancelled);
        }
        claimed.committed = true;
        if let Err(error) = self.transcript.commit_consumed(
            claimed.item.entry_id.clone(),
            message,
            record,
        ) {
            claimed.item.reject(&error);
            return Err(error);
        }
        match &claimed.item.waiter {
            Some(QueueWaiter::Insertion(notice)) => {
                notice.inserted(&message.identity.message_id)
            }
            Some(QueueWaiter::Receipt(receipt)) => {
                self.execution.accept_receipt(Arc::clone(receipt))?
            }
            None => {}
        }
        Ok(())
    }
}

/// Admission data kept together until the durable enqueue succeeds.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct QueueRequest {
    pub(super) kind: QueueKind,
    pub(super) input: protocol::RunInput,
    #[builder(default)]
    pub(super) waiter: Option<QueueWaiter>,
    #[builder(default)]
    pub(super) submission_id: Option<String>,
    #[builder(default)]
    pub(super) input_processed: bool,
    #[builder(default)]
    pub(super) cancellation: Option<tokio_util::sync::CancellationToken>,
}

/// Exactly one consumer owns the result of a queued input.
#[derive(Debug, Clone)]
pub(super) enum QueueWaiter {
    Insertion(Arc<InsertionNotice>),
    Receipt(Arc<super::submit::SubmissionReceipt>),
}

/// Returns a claim to its original lane unless persistence has already started.
pub(super) struct ClaimedItem {
    item: PendingQueueItem,
    execution: Arc<SessionExecution>,
    committed: bool,
}

impl ClaimedItem {
    /// Borrows the reserved message without transferring claim ownership.
    pub(super) fn item(&self) -> &PendingQueueItem {
        &self.item
    }
}

impl Drop for ClaimedItem {
    /// Restores unconsumed input even when preflight, hooks, or event delivery fail.
    fn drop(&mut self) {
        if !self.committed {
            match self.execution.queue.lock() {
                Ok(mut queue) => match self.item.queued.kind {
                    QueueKind::Steering => {
                        queue.steering.push_front(self.item.clone())
                    }
                    QueueKind::FollowUp => {
                        queue.follow_up.push_front(self.item.clone())
                    }
                },
                Err(error) => tracing::error!(
                    "failed returning claimed queue item {}: {}",
                    self.item.queued.queue_id,
                    error
                ),
            }
        }
    }
}

impl SessionExecution {
    /// Opens acceptance only for an agent Run, never for a command or maintenance operation.
    pub(super) fn open_queue(&self) -> Result<(), KernelError> {
        let cancellation = self
            .cancellation
            .lock()
            .map_err(|_error| KernelError::Poisoned)?;
        self.queue
            .lock()
            .map_err(|_error| KernelError::Poisoned)?
            .open = !cancellation.is_cancelled();
        Ok(())
    }

    /// Closes acceptance before settlement or cancellation withdraws pending inputs.
    pub(super) fn close_queue(&self) -> Result<(), KernelError> {
        self.queue
            .lock()
            .map_err(|_error| KernelError::Poisoned)?
            .open = false;
        Ok(())
    }

    /// Atomically claims eligible input or closes acceptance when no continuation remains.
    pub(super) fn claim_next_or_close(
        self: &Arc<Self>,
        allow_follow_up: bool,
    ) -> Result<Option<ClaimedItem>, KernelError> {
        let mut queue =
            self.queue.lock().map_err(|_error| KernelError::Poisoned)?;
        let item = queue.steering.pop_front().or_else(|| {
            allow_follow_up
                .then(|| queue.follow_up.pop_front())
                .flatten()
        });
        if item.is_none() && allow_follow_up {
            queue.open = false;
        }
        Ok(item.map(|item| ClaimedItem {
            item,
            execution: Arc::clone(self),
            committed: false,
        }))
    }
}

/// One queued message plus the entry identifier reserved before persistence.
#[derive(Debug, Clone)]
pub(super) struct PendingQueueItem {
    pub(super) queued: QueuedMessage,
    pub(super) entry_id: EntryId,
    pub(super) waiter: Option<QueueWaiter>,
}

impl PendingQueueItem {
    /// Creates an in-memory queue item after all public identifiers are validated.
    pub(super) fn new(queued: QueuedMessage, entry_id: EntryId) -> Self {
        Self {
            queued,
            entry_id,
            waiter: None,
        }
    }

    /// Delivers a definitive cancellation or an uncertainty error to the item's sole waiter.
    fn reject(&self, error: &KernelError) {
        match &self.waiter {
            Some(QueueWaiter::Insertion(notice)) => notice.rejected(error),
            Some(QueueWaiter::Receipt(receipt)) => {
                if matches!(error, KernelError::PromptCancelled) {
                    receipt.resolve(Arc::new(
                        super::SubmissionAnswer::cancelled(),
                    ));
                } else {
                    receipt.reject(error);
                }
            }
            None => {}
        }
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
    /// A live submitter receives input that the Run did not insert.
    Withdrawn,
}

/// In-memory view reconstructed from append-only queue records.
#[derive(Debug, Default)]
pub(super) struct PendingQueue {
    open: bool,
    steering: VecDeque<PendingQueueItem>,
    follow_up: VecDeque<PendingQueueItem>,
}

impl PendingQueue {
    /// Recovers queue records while excluding message entries already written before a crash.
    pub(super) fn restore(
        store: &dyn SessionStore,
    ) -> Result<Self, KernelError> {
        let mut queue = Self::from_records(&store.records())?;
        for id in queue.ids() {
            let item = queue.remove(&id).expect("id from locked queue");
            if store.get_entry(&item.entry_id).is_some() {
                tracing::warn!(
                    "ignored consumed queue item {} with existing entry {} during recovery",
                    id,
                    item.entry_id
                );
            } else {
                queue.push(item);
            }
        }
        Ok(queue)
    }

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
                    queue.remove(&data.queue_id);
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

    /// Reports whether both scheduling lanes contain no pending item.
    pub(super) fn is_empty(&self) -> bool {
        self.steering.is_empty() && self.follow_up.is_empty()
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
