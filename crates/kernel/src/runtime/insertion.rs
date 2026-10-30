use std::sync::{Arc, Mutex, PoisonError};

use protocol::MessageId;
use tokio::sync::oneshot;

use super::KernelError;

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
