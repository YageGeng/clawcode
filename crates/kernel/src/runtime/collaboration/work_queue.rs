use std::collections::VecDeque;

use tokio::sync::{Mutex, Notify};

/// Shared queue used by the supervisor and worker thread to exchange pending tasks.
pub(crate) struct AgentWorkQueue {
    state: Mutex<AgentWorkQueueState>,
    notify: Notify,
}

/// Mutable state protected by the work-queue mutex.
struct AgentWorkQueueState {
    pending: VecDeque<String>,
    closing: bool,
}

impl AgentWorkQueue {
    /// Creates an empty work queue for a mailbox-backed agent worker.
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(AgentWorkQueueState {
                pending: VecDeque::new(),
                closing: false,
            }),
            notify: Notify::new(),
        }
    }

    /// Queues a task either at the front or back of the agent work queue.
    pub(crate) async fn enqueue(&self, input: String, interrupt: bool) {
        let mut state = self.state.lock().await;
        if interrupt {
            state.pending.push_front(input);
        } else {
            state.pending.push_back(input);
        }
        drop(state);
        self.notify.notify_waiters();
    }

    /// Marks the queue as closed, drops queued work, and wakes blocked workers.
    pub(crate) async fn close(&self) {
        let mut state = self.state.lock().await;
        state.pending.clear();
        state.closing = true;
        drop(state);
        self.notify.notify_waiters();
    }

    /// Waits for the next queued task, returning `None` when the queue has been closed and drained.
    pub(crate) async fn next_input(&self) -> Option<String> {
        loop {
            let notified = {
                let mut state = self.state.lock().await;
                if let Some(input) = state.pending.pop_front() {
                    return Some(input);
                }
                if state.closing {
                    return None;
                }
                self.notify.notified()
            };
            notified.await;
        }
    }
}
