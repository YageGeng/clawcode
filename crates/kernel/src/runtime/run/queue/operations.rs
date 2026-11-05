//! Kernel and Session operations over the pending queue's data structures.

use std::sync::Arc;

use protocol::{
    AgentMessage, EntryId, IdKind, MessageIdentity, MessageTiming,
    PendingMessages, QueueId, QueueKind, QueuedMessage, RecordId, SessionId,
};
use store::{NewRecord, RecordKind};

use super::super::input::PromptInputExpansion;
use super::{
    ClaimedItem, PendingQueueItem, QueueCancellationReason, QueueRequest,
    QueueWaiter, cancelled_payload,
};
use crate::runtime::{EventEmitter, Kernel, KernelError, Session};

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
    pub(in crate::runtime) async fn enqueue(
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
                if !input_processed {
                    session.ensure_queueable(&input)?;
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
                            turn_id: self.new_turn_id()?,
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
        let mut queue = session.execution.queue.lock()?;
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
        let mut queue = session.execution.queue.lock()?;
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
    pub(in crate::runtime::run) fn publish_queue_changed(
        &self,
        session_id: &SessionId,
    ) {
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
                    let (turn_id, emitter) = kernel.observer_emitter(
                        Arc::clone(&publishing),
                        &session_id,
                    )?;
                    emitter
                        .emit(
                            turn_id,
                            protocol::AgentEventPayload::QueueChanged {
                                pending,
                            },
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
    pub(in crate::runtime) fn withdraw_pending(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        let session = self.session(session_id)?;
        let mut queue = session.execution.queue.lock()?;
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
    pub(in crate::runtime::run) fn queue_cancellation_record(
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
        let mut queue = self.execution.queue.lock()?;
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
    pub(in crate::runtime::run) fn remove_pending(
        &self,
        record: NewRecord,
        queue_id: &QueueId,
    ) -> Result<(), KernelError> {
        let mut queue = self.execution.queue.lock()?;
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
    pub(in crate::runtime::run) fn consume_claimed(
        &self,
        mut claimed: ClaimedItem,
        message: &AgentMessage,
        record: NewRecord,
    ) -> Result<(), KernelError> {
        // Cancel and the first write share this boundary, including multi-threaded callers.
        let cancellation = self.execution.cancellation.lock()?;
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
