mod operations;

use std::collections::VecDeque;
use std::sync::Arc;

use protocol::{EntryId, PendingMessages, QueueId, QueueKind, QueuedMessage};
use serde::{Deserialize, Serialize};
use store::{RecordKind, SessionRecord, SessionStore};

use super::insertion::SubmissionReceipt;
use crate::runtime::{InsertionNotice, KernelError, SessionExecution};

/// Placement of delegated input that arrives during an active run.
#[derive(Debug, Clone, Copy, Default)]
pub enum WhenBusy {
    Steer,
    #[default]
    FollowUp,
}

impl From<config::QueuePlacement> for WhenBusy {
    /// Converts persisted configuration into the runtime queue policy.
    fn from(value: config::QueuePlacement) -> Self {
        match value {
            config::QueuePlacement::Steer => Self::Steer,
            config::QueuePlacement::FollowUp => Self::FollowUp,
        }
    }
}

impl From<WhenBusy> for QueueKind {
    /// Maps delegated placement onto the existing durable queues.
    fn from(value: WhenBusy) -> Self {
        match value {
            WhenBusy::Steer => Self::Steering,
            WhenBusy::FollowUp => Self::FollowUp,
        }
    }
}

/// Admission data kept together until the durable enqueue succeeds.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct QueueRequest {
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
pub(in crate::runtime) enum QueueWaiter {
    Insertion(Arc<InsertionNotice>),
    Receipt(Arc<SubmissionReceipt>),
}

/// Returns a claim to its original lane unless persistence has already started.
pub(in crate::runtime) struct ClaimedItem {
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
        let cancellation = self.cancellation.lock()?;
        self.queue.lock()?.open = !cancellation.is_cancelled();
        Ok(())
    }

    /// Closes acceptance before settlement or cancellation withdraws pending inputs.
    pub(super) fn close_queue(&self) -> Result<(), KernelError> {
        self.queue.lock()?.open = false;
        Ok(())
    }

    /// Atomically claims eligible input or closes acceptance when no continuation remains.
    pub(super) fn claim_next_or_close(
        self: &Arc<Self>,
        allow_follow_up: bool,
    ) -> Result<Option<ClaimedItem>, KernelError> {
        let mut queue = self.queue.lock()?;
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
                        crate::runtime::SubmissionAnswer::cancelled(),
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
pub(in crate::runtime) struct PendingQueue {
    open: bool,
    steering: VecDeque<PendingQueueItem>,
    follow_up: VecDeque<PendingQueueItem>,
}

impl PendingQueue {
    /// Recovers queue records while excluding message entries already written before a crash.
    pub(in crate::runtime) fn restore(
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
