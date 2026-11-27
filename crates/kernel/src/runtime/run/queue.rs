mod operations;

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use protocol::{
    AgentMail, EntryId, ExtensionMessage, PendingMessages, QuestionKey,
    QuestionReply, QueueId, QueueKind, QueuedMessage,
};
use serde::{Deserialize, Serialize};
use store::{RecordKind, SessionRecord, SessionStore};

use crate::runtime::{InsertionNotice, Kernel, KernelError, SessionExecution};

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
    /// Client prompt that awaits this input's insertion.
    #[builder(default)]
    pub(super) waiter: Option<Arc<InsertionNotice>>,
    #[builder(default)]
    pub(super) submission_id: Option<String>,
    #[builder(default)]
    pub(super) input_processed: bool,
    #[builder(default)]
    pub(super) cancellation: Option<tokio_util::sync::CancellationToken>,
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
                Ok(mut queue) => {
                    // Restored answers are removable again. Their lane now owns the reservation.
                    for key in &self.item.question_keys {
                        queue.reply_keys.remove(key);
                    }
                    match self.item.queued.kind {
                        QueueKind::Steering => {
                            queue.steering.push_front(self.item.clone())
                        }
                        QueueKind::FollowUp => {
                            queue.follow_up.push_front(self.item.clone())
                        }
                    }
                }
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
        self.queue_opened.notify_waiters();
        Ok(())
    }

    /// Resolves once a Run next opens the queue; enable it before checking
    /// the queue so an opening in between is not missed.
    pub(in crate::runtime) fn queue_opened(
        &self,
    ) -> tokio::sync::futures::Notified<'_> {
        self.queue_opened.notified()
    }

    /// Closes acceptance before settlement or cancellation withdraws pending inputs.
    pub(super) fn close_queue(&self) -> Result<(), KernelError> {
        self.queue.lock()?.open = false;
        Ok(())
    }

    /// Atomically claims eligible input or closes acceptance when no continuation remains.
    ///
    /// A queued `new_task` mail keeps the queue open so the Run continues one
    /// more Turn to read it; queue-only mail never extends a finishing Run.
    pub(super) fn claim_next_or_close(
        self: &Arc<Self>,
        allow_follow_up: bool,
    ) -> Result<ClaimOutcome, KernelError> {
        let mut queue = self.queue.lock()?;
        let item = queue.steering.pop_front().or_else(|| {
            allow_follow_up
                .then(|| queue.follow_up.pop_front())
                .flatten()
        });
        if let Some(item) = item {
            // A claimed reply is in neither the queue nor the transcript until
            // it commits, so its questions stay closed through `reply_keys`.
            queue.reply_keys.extend(item.question_keys.iter().cloned());
            return Ok(ClaimOutcome::Item(Box::new(ClaimedItem {
                item,
                execution: Arc::clone(self),
                committed: false,
            })));
        }
        if !allow_follow_up {
            return Ok(ClaimOutcome::Done);
        }
        if queue.mail.iter().any(|mail| mail.kind.triggers_turn()) {
            return Ok(ClaimOutcome::Continue);
        }
        queue.open = false;
        Ok(ClaimOutcome::Done)
    }

    /// Queues mail for the active Run, or hands it back when no Run accepts input.
    pub(in crate::runtime) fn queue_mail(
        &self,
        mail: AgentMail,
    ) -> Result<Option<AgentMail>, KernelError> {
        let mut queue = self.queue.lock()?;
        if !queue.open {
            return Ok(Some(mail));
        }
        queue.mail.push_back(mail);
        drop(queue);
        self.mailbox.send_modify(|activity| {
            activity.mail = activity.mail.wrapping_add(1);
        });
        Ok(None)
    }

    /// Keeps a Run's first-input reply closed until the Run ends; the input
    /// reaches the transcript only in the first Turn.
    pub(in crate::runtime) fn reserve_reply_keys(
        &self,
        reply: &QuestionReply,
    ) -> Result<(), KernelError> {
        self.queue.lock()?.reply_keys.extend(reply.keys());
        Ok(())
    }

    /// Returns every question settled by input that is not yet in the
    /// transcript: queued replies and replies the active Run holds.
    pub(in crate::runtime) fn settled_question_keys(
        &self,
    ) -> Result<BTreeSet<QuestionKey>, KernelError> {
        Ok(self.queue.lock()?.reserved_questions())
    }

    /// Holds a skip for the active Run, or hands it back when no Run accepts input.
    pub(in crate::runtime) fn queue_skip(
        &self,
        skip: QuestionReply,
    ) -> Result<Option<QuestionReply>, KernelError> {
        let mut queue = self.queue.lock()?;
        if !queue.open {
            return Ok(Some(skip));
        }
        queue.skips.push_back(skip);
        Ok(None)
    }

    /// Removes every held skip in arrival order for writing.
    pub(in crate::runtime) fn take_skips(
        &self,
    ) -> Result<Vec<ExtensionMessage>, KernelError> {
        let mut queue = self.queue.lock()?;
        let skips = std::mem::take(&mut queue.skips);
        // Keep the keys reserved until the Run finishes the transcript write.
        queue
            .reply_keys
            .extend(skips.iter().flat_map(QuestionReply::keys));
        drop(queue);
        skips
            .iter()
            .map(ExtensionMessage::try_from)
            .collect::<Result<_, _>>()
            .map_err(|error| KernelError::Protocol(error.to_string()))
    }

    /// Removes every queued mail in arrival order.
    pub(in crate::runtime) fn take_mail(
        &self,
    ) -> Result<Vec<AgentMail>, KernelError> {
        Ok(self.queue.lock()?.mail.drain(..).collect())
    }

    /// Reports whether mail is waiting for the next model request.
    pub(in crate::runtime) fn pending_mail(&self) -> Result<bool, KernelError> {
        Ok(!self.queue.lock()?.mail.is_empty())
    }
}

/// Releases the Run's in-flight question reservations on success, failure or cancellation.
pub(super) struct RunQuestionReservations<'a> {
    pub(super) kernel: &'a Kernel,
    pub(super) session_id: &'a protocol::SessionId,
    pub(super) execution: &'a SessionExecution,
}

impl Drop for RunQuestionReservations<'_> {
    /// Publishes released keys so observers can reopen questions whose writes failed.
    fn drop(&mut self) {
        match self.execution.queue.lock() {
            Ok(mut queue) => {
                // A concurrent dismissal must either be released here or see
                // a closed queue and persist the skip after the Run exits.
                queue.open = false;
                let changed =
                    !queue.reply_keys.is_empty() || !queue.skips.is_empty();
                let abandoned = std::mem::take(&mut queue.skips);
                queue.reply_keys.clear();
                drop(queue);
                if !abandoned.is_empty() {
                    tracing::warn!(
                        "released {} unwritten question skips at Run exit for Session {}",
                        abandoned.len(),
                        self.session_id
                    );
                }
                if changed {
                    self.kernel.publish_queue_changed(self.session_id);
                }
            }
            Err(error) => tracing::error!(
                "failed releasing question reservations for session {}: {}",
                self.session_id,
                error
            ),
        }
    }
}

/// What a finishing Turn found to continue with.
pub(in crate::runtime) enum ClaimOutcome {
    /// A user queue item becomes the next Turn's input.
    Item(Box<ClaimedItem>),
    /// Queued `new_task` mail needs one more Turn without new user input.
    Continue,
    /// Nothing continues the Run from the queue.
    Done,
}

impl ClaimOutcome {
    /// Returns the claimed user item, if this outcome carries one.
    pub(in crate::runtime) fn into_item(self) -> Option<ClaimedItem> {
        match self {
            Self::Item(item) => Some(*item),
            Self::Continue | Self::Done => None,
        }
    }
}

/// One queued message plus the entry identifier reserved before persistence.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub(super) struct PendingQueueItem {
    pub(super) queued: QueuedMessage,
    pub(super) entry_id: EntryId,
    #[builder(default)]
    pub(super) waiter: Option<Arc<InsertionNotice>>,
    /// Reservation reads borrow these identities without decoding answers under the queue lock.
    question_keys: Vec<QuestionKey>,
}

impl PendingQueueItem {
    /// Delivers a definitive cancellation or an uncertainty error to the item's sole waiter.
    fn reject(&self, error: &KernelError) {
        if let Some(notice) = &self.waiter {
            notice.rejected(error);
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
    /// Inter-agent mail held in memory until the next model request or Run close.
    // ponytail: mail of a busy recipient lives only in memory and is lost with
    // a crashed Run; write lane records when crash recovery is required.
    mail: VecDeque<AgentMail>,
    /// Questions held by the active Run's first or claimed reply, or by a
    /// skip taken for writing. Cleared when the Run ends.
    reply_keys: BTreeSet<QuestionKey>,
    /// Question skips for the active Run, written beside non-waking mail so a
    /// skip never extends the Run.
    // ponytail: skips of a busy Session live only in memory and are lost with
    // a crashed Run, which reopens their questions; write lane records if that matters.
    skips: VecDeque<QuestionReply>,
}

impl PendingQueue {
    /// Recovers queue records while excluding message entries already written before a crash.
    pub(in crate::runtime) fn restore(
        store: &dyn SessionStore,
        records: &[SessionRecord],
    ) -> Result<Self, KernelError> {
        let mut queue = Self::from_records(records)?;
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
                    queue.push(PendingQueueItem::from(data));
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
                | RecordKind::Usage
                | RecordKind::RequestContent
                | RecordKind::LegacyFileApprovalMode => {}
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

    /// Reads all question reservations under the same lock as the public queue lanes.
    fn reserved_questions(&self) -> BTreeSet<QuestionKey> {
        let mut keys = self.reply_keys.clone();
        for item in self.steering.iter().chain(&self.follow_up) {
            keys.extend(item.question_keys.iter().cloned());
        }
        keys.extend(self.skips.iter().flat_map(QuestionReply::keys));
        keys
    }

    /// Clones the public queue view without exposing reserved entry identifiers.
    pub(super) fn snapshot(&self) -> PendingMessages {
        PendingMessages::builder()
            .reserved_questions(self.reserved_questions())
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

impl From<QueueEnqueuedData> for PendingQueueItem {
    /// Rebuilds reservations once when reopening a durable queue, before any caller locks it.
    fn from(data: QueueEnqueuedData) -> Self {
        let keys = QuestionReply::try_from(&data.queue.message.content)
            .map(|reply| reply.keys().collect())
            .unwrap_or_default();
        Self::builder()
            .queued(data.queue)
            .entry_id(data.entry_id)
            .question_keys(keys)
            .build()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueCancelledData {
    queue_id: QueueId,
    reason: QueueCancellationReason,
}
