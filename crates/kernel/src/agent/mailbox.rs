//! Inter-agent message mailbox.
//!
//! Each session has a `Mailbox` (send side) and a `MailboxReceiver` (recv side).
//! Messages are delivered via an unbounded mpsc channel with a sequence counter
//! and a `watch` channel for wake notifications.

use std::sync::atomic::{AtomicU64, Ordering};

use protocol::InterAgentMessage;
use tokio::sync::{mpsc, watch};

/// Send side of an agent mailbox.
///
/// Cloned manually — `AtomicU64` does not implement `Clone`.
pub(crate) struct Mailbox {
    tx: mpsc::UnboundedSender<InterAgentMessage>,
    seq: AtomicU64,
    wake: watch::Sender<u64>,
}

/// Receive side of an agent mailbox.
#[allow(dead_code)]
pub(crate) struct MailboxReceiver {
    rx: mpsc::UnboundedReceiver<InterAgentMessage>,
    wake_rx: watch::Receiver<u64>,
    read_seq: AtomicU64,
}

impl Mailbox {
    /// Send a message, incrementing the sequence counter and waking
    /// any waiters.
    pub(crate) fn send(&self, msg: InterAgentMessage) {
        let seq = self.seq.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.tx.send(msg);
        let _ = self.wake.send(seq);
    }

    /// Return a wake receiver for use by `wait_agent`.
    #[allow(dead_code)]
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.wake.subscribe()
    }
}

impl Clone for Mailbox {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            seq: AtomicU64::new(self.seq.load(Ordering::Acquire)),
            wake: self.wake.clone(),
        }
    }
}

#[allow(dead_code)]
impl MailboxReceiver {
    /// Drain all pending messages from the channel.
    /// Returns the collected messages and updates the read sequence.
    pub(crate) fn drain(&mut self) -> Vec<InterAgentMessage> {
        let mut msgs = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            msgs.push(msg);
        }
        self.read_seq
            .store(*self.wake_rx.borrow(), Ordering::Release);
        msgs
    }

    /// Check whether any pending message has `trigger_turn` set.
    /// This uses the wake signal as a heuristic — the actual
    /// trigger_turn check happens after drain() collects the messages.
    pub(crate) fn has_pending_trigger_turn(&self) -> bool {
        let latest = *self.wake_rx.borrow();
        let read = self.read_seq.load(Ordering::Acquire);
        latest > read
    }
}

/// Create a linked pair of mailbox endpoints.
pub(crate) fn mailbox_pair() -> (Mailbox, MailboxReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (wake_tx, wake_rx) = watch::channel(0);
    (
        Mailbox {
            tx,
            seq: AtomicU64::new(0),
            wake: wake_tx,
        },
        MailboxReceiver {
            rx,
            wake_rx,
            read_seq: AtomicU64::new(0),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::agent::AgentPath;

    fn test_msg(content: &str, trigger: bool) -> InterAgentMessage {
        InterAgentMessage::builder()
            .from(AgentPath::root())
            .to(AgentPath::root().join("child"))
            .content(content.to_string())
            .trigger_turn(trigger)
            .build()
    }

    #[test]
    fn send_and_drain() {
        let (mb, mut rx) = mailbox_pair();
        mb.send(test_msg("hello", false));
        mb.send(test_msg("world", false));

        let msgs = rx.drain();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].content, "world");
    }

    #[test]
    fn empty_drain_returns_nothing() {
        let (_, mut rx) = mailbox_pair();
        let msgs = rx.drain();
        assert!(msgs.is_empty());
    }

    #[test]
    fn wake_signal_updates_on_send() {
        let (mb, rx) = mailbox_pair();
        assert!(!rx.has_pending_trigger_turn());
        mb.send(test_msg("go", true));
        assert!(rx.has_pending_trigger_turn());
    }
}
