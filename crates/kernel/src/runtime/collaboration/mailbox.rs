use std::collections::{HashSet, VecDeque};

use tokio::sync::{Mutex, watch};

use tools::MailboxEvent;

/// Mailbox that stores unread ancestor notifications and a sequence watcher for waiters.
pub(crate) struct AgentMailbox {
    state: Mutex<AgentMailboxState>,
    updates: watch::Sender<u64>,
}

/// Mutable mailbox state protected by the mailbox mutex.
struct AgentMailboxState {
    sequence: u64,
    events: VecDeque<MailboxEvent>,
}

impl AgentMailbox {
    /// Creates an empty mailbox with an initial zero-valued update sequence.
    pub(crate) fn new() -> Self {
        let (updates, _) = watch::channel(0);
        Self {
            state: Mutex::new(AgentMailboxState {
                sequence: 0,
                events: VecDeque::new(),
            }),
            updates,
        }
    }

    /// Appends one unread mailbox event and wakes all active waiters.
    pub(crate) async fn push(&self, event: MailboxEvent) {
        let next_sequence = {
            let mut state = self.state.lock().await;
            state.events.push_back(event);
            state.sequence += 1;
            state.sequence
        };
        let _ = self.updates.send(next_sequence);
    }

    /// Removes and returns the first unread event that matches the supplied target set.
    pub(crate) async fn pop_matching(&self, targets: &HashSet<String>) -> Option<MailboxEvent> {
        let mut state = self.state.lock().await;
        let index = state
            .events
            .iter()
            .position(|event| targets.is_empty() || targets.contains(&event.agent_id))?;
        state.events.remove(index)
    }

    /// Returns the number of unread events still buffered in the mailbox.
    pub(crate) async fn unread_len(&self) -> usize {
        self.state.lock().await.events.len()
    }

    /// Subscribes to the mailbox update sequence so waiters can sleep efficiently.
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.updates.subscribe()
    }
}
