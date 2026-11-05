//! One-shot waiters that report an input's outcome back to whoever submitted it.

use std::sync::{Arc, Mutex, PoisonError};

use protocol::{AgentMessage, AgentOutcome, MessageContent, MessageId};
use tokio::sync::oneshot;

use crate::runtime::KernelError;

/// Receiver half that resolves once a Run inserts its user message or fails first.
pub type InsertionReceiver = oneshot::Receiver<Result<MessageId, KernelError>>;

/// One-shot report of whether a Run inserted its user message (ACP v2 prompt acceptance).
#[derive(Debug)]
pub struct InsertionNotice(
    Mutex<Option<oneshot::Sender<Result<MessageId, KernelError>>>>,
);

impl InsertionNotice {
    /// Creates a notice and the receiver the protocol adapter awaits.
    #[must_use]
    pub fn new() -> (Arc<Self>, InsertionReceiver) {
        let (sender, receiver) = oneshot::channel();
        (Arc::new(Self(Mutex::new(Some(sender)))), receiver)
    }

    /// Reports whether a client still awaits this input's acceptance.
    pub fn is_listening(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|sender| !sender.is_closed())
    }

    /// Reports insertion once; later calls are ignored.
    pub(in crate::runtime) fn inserted(&self, message_id: &MessageId) {
        if self.settle(Ok(message_id.clone())) {
            tracing::info!(
                "reported prompt insertion of message {}",
                message_id
            );
        }
    }

    /// Reports a failure that ended the Run before insertion; ignored after insertion.
    pub(in crate::runtime) fn rejected(&self, error: &KernelError) {
        if self.settle(Err(error.for_insertion())) {
            tracing::info!("rejected prompt before insertion: {}", error);
        }
    }

    /// Delivers the first outcome and reports whether this call was that first outcome.
    fn settle(&self, outcome: Result<MessageId, KernelError>) -> bool {
        let sender =
            self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(sender) = sender else {
            return false;
        };
        // A dropped receiver means the requester left; the Run still owns its result.
        if sender.send(outcome).is_err() {
            tracing::debug!(
                "prompt insertion receiver was dropped before settlement"
            );
        }
        true
    }
}

/// Whether a session-wide cancel may reject a Run still waiting before insertion.
#[derive(Debug, Clone)]
pub(in crate::runtime) enum WaiterCancellation {
    /// A client awaits acceptance and still holds the input after a `-32800` rejection.
    Reject,
    /// Internal input has no client to return it to, so it keeps its place.
    Ignore,
    /// Cancellation generation captured at protocol arrival, before asynchronous hooks.
    Captured(tokio_util::sync::CancellationToken),
}

impl WaiterCancellation {
    /// Maps a close observed before admission to this waiter's rejection.
    pub(in crate::runtime) fn closing(&self) -> KernelError {
        match self {
            Self::Reject | Self::Captured(_) => KernelError::PromptCancelled,
            Self::Ignore => KernelError::SessionClosing,
        }
    }
}

impl From<Option<&InsertionNotice>> for WaiterCancellation {
    /// Only Runs with an external client waiting for acceptance may be rejected by a session cancel.
    fn from(notice: Option<&InsertionNotice>) -> Self {
        match notice {
            Some(_) => Self::Reject,
            None => Self::Ignore,
        }
    }
}

/// Terminal state of the run that answered delegated input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerOutcome {
    Completed,
    Cancelled,
    Failed(String),
}

/// Last assistant answer, without duplicating the child Session's usage.
#[derive(Debug, Clone)]
pub struct SubmissionAnswer {
    /// Stable assistant identity used to deduplicate background reports.
    pub message_id: Option<MessageId>,
    /// Concatenated text blocks from the last assistant message.
    pub text: String,
    /// Outcome of the answering run.
    pub outcome: AnswerOutcome,
}

type ReceiptResult = Result<Arc<SubmissionAnswer>, KernelError>;

/// One in-memory submission receipt, settled exactly once by consumption or withdrawal.
#[derive(Debug)]
pub(in crate::runtime) struct SubmissionReceipt {
    sender: Mutex<Option<oneshot::Sender<ReceiptResult>>>,
}

impl SubmissionReceipt {
    /// Creates a receipt before enqueueing so even immediate consumption cannot lose its answer.
    pub(super) fn new() -> (Arc<Self>, oneshot::Receiver<ReceiptResult>) {
        let (sender, receiver) = oneshot::channel();
        (
            Arc::new(Self {
                sender: Mutex::new(Some(sender)),
            }),
            receiver,
        )
    }

    /// Completes a receipt once; a dropped caller does not invalidate the persisted run.
    pub(super) fn resolve(&self, answer: Arc<SubmissionAnswer>) {
        let mut sender = self.sender.lock().unwrap_or_else(|error| {
            tracing::error!(
                "submission receipt lock was poisoned; releasing its waiter"
            );
            error.into_inner()
        });
        if let Some(sender) = sender.take() {
            let _ = sender.send(Ok(answer));
        }
    }
    /// Reports uncertainty without disguising an unknown outcome as cancelled input.
    pub(super) fn reject(&self, error: &KernelError) {
        if let Some(sender) = self
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(Err(error.for_insertion()));
        }
    }
}

impl SubmissionAnswer {
    /// Represents input withdrawn before an answering run can complete.
    pub(super) fn cancelled() -> Self {
        Self {
            message_id: None,
            text: String::new(),
            outcome: AnswerOutcome::Cancelled,
        }
    }
}

impl From<(&AgentOutcome, &[AgentMessage])> for SubmissionAnswer {
    /// Uses the final Run outcome and its own messages, never the Session's newest history.
    fn from((outcome, messages): (&AgentOutcome, &[AgentMessage])) -> Self {
        // Native provenance does not change the assistant role or its lifecycle.
        let message = messages.iter().rev().find(|message| {
            matches!(message.content, MessageContent::Assistant { .. })
        });
        Self {
            message_id: message
                .map(|message| message.identity.message_id.clone()),
            text: message
                .and_then(|message| message.content.text_content())
                .unwrap_or_default(),
            outcome: match outcome {
                AgentOutcome::Succeeded => AnswerOutcome::Completed,
                AgentOutcome::Cancelled => AnswerOutcome::Cancelled,
                AgentOutcome::Failed { message } => {
                    AnswerOutcome::Failed(message.clone())
                }
            },
        }
    }
}
