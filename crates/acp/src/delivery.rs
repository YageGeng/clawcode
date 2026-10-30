use std::sync::Arc;

use protocol::Sequence;
use tokio::sync::{Mutex, MutexGuard, watch};

/// One connection's confirmation that its Session watcher queued notifications through a sequence.
///
/// Confirmation covers the SDK outbound queue of this connection only; it does not
/// require a client receipt.
#[derive(Clone)]
pub(crate) struct SessionDelivery {
    progress: watch::Receiver<Option<Sequence>>,
    sending: Arc<Mutex<()>>,
}

/// Watcher-side half that publishes progress after each group is queued.
pub(crate) struct DeliveryPublisher {
    progress: watch::Sender<Option<Sequence>>,
    sending: Arc<Mutex<()>>,
}

/// Exclusive hold on a watcher's sending; while held, the connection queues no new notifications.
pub(crate) struct SendPermit<'a> {
    _guard: MutexGuard<'a, ()>,
}

impl SessionDelivery {
    /// Creates a connected pair whose progress starts at the watcher's subscription boundary.
    pub(crate) fn new(
        delivered: Option<Sequence>,
    ) -> (Self, DeliveryPublisher) {
        let (sender, receiver) = watch::channel(delivered);
        let sending = Arc::new(Mutex::new(()));
        (
            Self {
                progress: receiver,
                sending: Arc::clone(&sending),
            },
            DeliveryPublisher {
                progress: sender,
                sending,
            },
        )
    }

    /// Waits until every notification through `sequence` is queued on this connection.
    pub(crate) async fn wait_through(
        &self,
        sequence: Sequence,
    ) -> Result<(), agent_client_protocol::Error> {
        let mut progress = self.progress.clone();
        match progress
            .wait_for(|delivered| {
                delivered.is_some_and(|sent| sent >= sequence)
            })
            .await
        {
            Ok(_delivered) => Ok(()),
            Err(_closed) => {
                tracing::warn!(
                    "ACP Session watcher stopped before delivering sequence {}",
                    sequence
                );
                Err(agent_client_protocol::Error::internal_error()
                    .data("Session watcher stopped before delivering updates"))
            }
        }
    }

    /// Blocks new watcher sends; acquire only after `wait_through`, or the watcher cannot advance.
    pub(crate) async fn send_permit(&self) -> SendPermit<'_> {
        SendPermit {
            _guard: self.sending.lock().await,
        }
    }
}

impl DeliveryPublisher {
    /// Holds the send lock while the watcher queues one group of notifications.
    pub(crate) async fn sending(&self) -> MutexGuard<'_, ()> {
        self.sending.lock().await
    }

    /// Publishes that every notification through `sequence` is queued.
    pub(crate) fn sent_through(&self, sequence: Sequence) {
        self.progress.send_if_modified(|delivered| {
            if delivered.is_some_and(|sent| sent >= sequence) {
                return false;
            }
            *delivered = Some(sequence);
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use protocol::Sequence;

    use super::SessionDelivery;

    /// Builds one test sequence.
    fn sequence(value: u64) -> Sequence {
        Sequence::try_from(value).expect("sequence")
    }

    /// A response barrier waits until the watcher publishes the target sequence.
    #[tokio::test]
    async fn wait_through_blocks_until_sequence_is_published() {
        let (delivery, publisher) = SessionDelivery::new(Some(sequence(5)));
        delivery
            .wait_through(sequence(5))
            .await
            .expect("boundary already delivered");
        let waiting = delivery.clone();
        let waiter =
            tokio::spawn(
                async move { waiting.wait_through(sequence(7)).await },
            );
        publisher.sent_through(sequence(6));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        publisher.sent_through(sequence(7));
        waiter.await.expect("join").expect("delivered");
    }

    /// A dead watcher cannot leave a response barrier pending forever.
    #[tokio::test]
    async fn wait_through_fails_when_publisher_drops() {
        let (delivery, publisher) = SessionDelivery::new(None);
        let waiting = delivery.clone();
        let waiter =
            tokio::spawn(
                async move { waiting.wait_through(sequence(1)).await },
            );
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(publisher);
        waiter
            .await
            .expect("join")
            .expect_err("closed watcher must not confirm delivery");
    }

    /// Holding the send permit keeps the watcher from publishing until it is released.
    #[tokio::test]
    async fn send_permit_excludes_watcher_publish() {
        let (delivery, publisher) = SessionDelivery::new(None);
        let permit = delivery.send_permit().await;
        let sending = tokio::spawn(async move {
            {
                let _sending = publisher.sending().await;
                publisher.sent_through(sequence(1));
            }
            publisher
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!sending.is_finished());
        drop(permit);
        let _publisher = sending.await.expect("join");
        delivery
            .wait_through(sequence(1))
            .await
            .expect("published after permit release");
    }
}
