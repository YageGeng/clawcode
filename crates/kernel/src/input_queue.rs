//! Session-scoped input queue for inter-agent mailbox delivery.
//!
//! This mirrors the reference design's separation between incoming mailbox communication and
//! turn execution: messages can be queued while a turn is active, trigger-turn
//! items are selected at turn boundaries, and queue-only items are injected
//! without starting a new turn.

use std::collections::VecDeque;

use protocol::InterAgentMessage;
use tokio::sync::watch;
use tools::builtin::agents::MailboxActivitySubscription;

/// Tracks mailbox activity independently from message delivery into context.
struct MailboxActivity {
    tx: watch::Sender<u64>,
    current_epoch: u64,
    observed_epoch: u64,
}

impl Default for MailboxActivity {
    /// Create a mailbox activity cursor with no observed or pending activity.
    fn default() -> Self {
        let (tx, _) = watch::channel(0);
        Self {
            tx,
            current_epoch: 0,
            observed_epoch: 0,
        }
    }
}

impl MailboxActivity {
    /// Record one new mailbox activity and notify live waiters.
    fn record(&mut self) {
        self.current_epoch = self.current_epoch.saturating_add(1);
        self.tx.send_replace(self.current_epoch);
    }

    /// Build a subscription over the current activity cursor state.
    fn subscribe(&self) -> MailboxActivitySubscription {
        MailboxActivitySubscription::new(
            self.tx.subscribe(),
            self.current_epoch,
            self.observed_epoch,
        )
    }

    /// Mark activity up to `epoch` as observed by wait_agent.
    fn mark_observed(&mut self, epoch: u64) {
        self.observed_epoch =
            self.observed_epoch.max(epoch.min(self.current_epoch));
    }
}

/// Stores inter-agent messages waiting for model-visible delivery.
pub(crate) struct InputQueue {
    mailbox_activity: MailboxActivity,
    mailbox_pending_mails: VecDeque<InterAgentMessage>,
}

impl Default for InputQueue {
    /// Create an empty input queue with a mailbox notification channel.
    fn default() -> Self {
        Self {
            mailbox_activity: MailboxActivity::default(),
            mailbox_pending_mails: VecDeque::new(),
        }
    }
}

impl InputQueue {
    /// Subscribe to mailbox delivery notifications.
    ///
    /// The returned cursor can complete immediately when activity is already
    /// pending or has already been delivered into model context but not observed.
    pub(crate) fn subscribe_mailbox(&self) -> MailboxActivitySubscription {
        self.mailbox_activity.subscribe()
    }

    /// Mark mailbox activity up to `epoch` as observed by wait_agent.
    pub(crate) fn mark_mailbox_observed(&mut self, epoch: u64) {
        self.mailbox_activity.mark_observed(epoch);
    }

    /// Queue an inter-agent message for the next eligible delivery phase.
    pub(crate) fn enqueue_mailbox_communication(
        &mut self,
        message: InterAgentMessage,
    ) {
        self.mailbox_pending_mails.push_back(message);
        self.mailbox_activity.record();
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
    fn input_queue_tracks_each_mailbox_activity_epoch() {
        let mut queue = InputQueue::default();

        assert_eq!(queue.subscribe_mailbox().current_epoch(), 0);
        queue.enqueue_mailbox_communication(test_message("first", false));
        queue.enqueue_mailbox_communication(test_message("second", false));

        assert_eq!(queue.subscribe_mailbox().current_epoch(), 2);
    }

    #[test]
    fn input_queue_marks_delivered_activity_observed_once_for_wait_agent() {
        let mut queue = InputQueue::default();
        queue.enqueue_mailbox_communication(test_message("delivered", false));

        let messages = queue.drain_mailbox_input_items();
        assert_eq!(messages.len(), 1);

        let delivered_rx = queue.subscribe_mailbox();
        assert!(
            delivered_rx.has_unobserved_activity(),
            "wait_agent should complete once for mail that was just delivered into context"
        );
        queue.mark_mailbox_observed(delivered_rx.current_epoch());
        let fresh_rx = queue.subscribe_mailbox();
        assert!(
            !fresh_rx.has_unobserved_activity(),
            "observed mail should not complete unrelated future waits"
        );
    }

    #[test]
    fn input_queue_marks_subscriber_changed_for_pending_mail() {
        let mut queue = InputQueue::default();
        queue.enqueue_mailbox_communication(test_message("pending", false));

        let mailbox_rx = queue.subscribe_mailbox();

        assert!(mailbox_rx.has_unobserved_activity());
    }
}
