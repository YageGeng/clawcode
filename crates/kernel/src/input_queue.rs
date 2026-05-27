//! Session-scoped input queue for inter-agent mailbox delivery.
//!
//! This mirrors Codex's separation between incoming mailbox communication and
//! turn execution: messages can be queued while a turn is active, trigger-turn
//! items are selected at turn boundaries, and queue-only items are injected
//! without starting a new turn.

use std::collections::VecDeque;

use protocol::InterAgentMessage;
use tokio::sync::watch;

/// Stores inter-agent messages waiting for model-visible delivery.
pub(crate) struct InputQueue {
    mailbox_tx: watch::Sender<()>,
    mailbox_pending_mails: VecDeque<InterAgentMessage>,
}

impl Default for InputQueue {
    /// Create an empty input queue with a mailbox notification channel.
    fn default() -> Self {
        let (mailbox_tx, _) = watch::channel(());
        Self {
            mailbox_tx,
            mailbox_pending_mails: VecDeque::new(),
        }
    }
}

impl InputQueue {
    /// Subscribe to mailbox delivery notifications.
    ///
    /// Marking the receiver as changed when mail is already pending mirrors
    /// Codex V2 and lets wait_agent complete for queued-but-undelivered mail.
    pub(crate) fn subscribe_mailbox(&self) -> watch::Receiver<()> {
        let mut mailbox_rx = self.mailbox_tx.subscribe();
        if self.has_pending_mailbox_items() {
            mailbox_rx.mark_changed();
        }
        mailbox_rx
    }

    /// Queue an inter-agent message for the next eligible delivery phase.
    pub(crate) fn enqueue_mailbox_communication(
        &mut self,
        message: InterAgentMessage,
    ) {
        self.mailbox_pending_mails.push_back(message);
        self.mailbox_tx.send_replace(());
    }

    /// Return whether any mailbox messages are waiting for model-visible delivery.
    pub(crate) fn has_pending_mailbox_items(&self) -> bool {
        !self.mailbox_pending_mails.is_empty()
    }

    /// Remove the next queued message that requested a follow-up turn.
    pub(crate) fn take_next_triggering_message(
        &mut self,
    ) -> Option<InterAgentMessage> {
        let index = self
            .mailbox_pending_mails
            .iter()
            .position(|message| message.trigger_turn)?;
        self.mailbox_pending_mails.remove(index)
    }

    /// Drain all queued inter-agent messages in delivery order.
    pub(crate) fn drain_mailbox_input_items(
        &mut self,
    ) -> Vec<InterAgentMessage> {
        self.mailbox_pending_mails.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::AgentPath;

    /// Build a test inter-agent message with the requested trigger flag.
    fn test_message(content: &str, trigger_turn: bool) -> InterAgentMessage {
        InterAgentMessage::builder()
            .from(AgentPath::root())
            .to(AgentPath::root().join("child"))
            .content(content.to_string())
            .trigger_turn(trigger_turn)
            .build()
    }

    #[test]
    fn input_queue_removes_first_trigger_only() {
        let mut queue = InputQueue::default();
        queue.enqueue_mailbox_communication(test_message("first", false));
        queue.enqueue_mailbox_communication(test_message("second", true));
        queue.enqueue_mailbox_communication(test_message("third", true));

        let message = queue
            .take_next_triggering_message()
            .expect("triggering message");
        let remaining = queue.drain_mailbox_input_items();

        assert_eq!(message.content, "second");
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].content, "first");
        assert_eq!(remaining[1].content, "third");
    }

    #[test]
    fn input_queue_drains_mailbox_in_delivery_order() {
        let mut queue = InputQueue::default();
        queue.enqueue_mailbox_communication(test_message("one", false));
        queue.enqueue_mailbox_communication(test_message("two", false));

        let messages = queue.drain_mailbox_input_items();

        assert_eq!(messages[0].content, "one");
        assert_eq!(messages[1].content, "two");
        assert!(queue.drain_mailbox_input_items().is_empty());
    }

    #[test]
    fn input_queue_marks_subscriber_changed_for_pending_mail() {
        let mut queue = InputQueue::default();
        queue.enqueue_mailbox_communication(test_message("pending", false));

        let mailbox_rx = queue.subscribe_mailbox();

        assert!(mailbox_rx.has_changed().expect("mailbox watch open"));
    }
}
