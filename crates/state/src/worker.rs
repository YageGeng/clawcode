//! Background catch-up of Sessions after each durable store sync.
//!
//! Stores call [`store::SyncObserver`] while their owners may hold locks, so
//! the observer only enqueues; this task merges queued events per Session and
//! refreshes each one, keeping metadata and history eventually consistent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocol::SessionId;
use tokio::sync::{mpsc, oneshot};

use crate::StateRuntime;

/// One queued request for the worker.
enum WorkerMessage {
    /// A Session log became durable.
    Synced(SessionId, PathBuf),
    /// Answer once every earlier message has been processed.
    Flush(oneshot::Sender<()>),
}

/// Sync observer that projects Sessions on a background task.
pub struct StateWorker {
    sender: mpsc::UnboundedSender<WorkerMessage>,
}

impl StateWorker {
    /// Starts the background task; it stops when every handle is dropped.
    pub fn spawn(
        runtime: Arc<StateRuntime>,
    ) -> (Arc<Self>, tokio::task::JoinHandle<()>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(WorkerLoop { runtime, receiver }.run());
        (Arc::new(Self { sender }), task)
    }

    /// Waits until every event queued before this call has been projected.
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        if self.sender.send(WorkerMessage::Flush(done)).is_ok() {
            // A dropped worker has nothing left to flush.
            let _ = wait.await;
        }
    }
}

impl store::SyncObserver for StateWorker {
    /// Enqueues a catch-up without blocking the syncing store.
    fn synced(&self, session_id: &SessionId, path: &Path) {
        // A stopped worker means the process is shutting down; the next backfill repairs.
        let _ = self.sender.send(WorkerMessage::Synced(
            session_id.clone(),
            path.to_path_buf(),
        ));
    }
}

/// Receiving side of the worker channel.
struct WorkerLoop {
    runtime: Arc<StateRuntime>,
    receiver: mpsc::UnboundedReceiver<WorkerMessage>,
}

impl WorkerLoop {
    /// Processes batches until the channel closes.
    async fn run(mut self) {
        while let Some(first) = self.receiver.recv().await {
            let mut pending = BTreeMap::new();
            let mut flushes = Vec::new();
            let mut message = Some(first);
            // Merge everything already queued so a burst of syncs costs one refresh each.
            while let Some(next) = message {
                match next {
                    WorkerMessage::Synced(id, path) => {
                        pending.insert(id, path);
                    }
                    WorkerMessage::Flush(done) => flushes.push(done),
                }
                message = self.receiver.try_recv().ok();
            }
            for (id, path) in pending {
                if let Err(error) = self.runtime.refresh(&id, &path).await {
                    tracing::warn!(
                        "failed to project session {} after sync: {}",
                        id,
                        error
                    );
                }
            }
            for done in flushes {
                let _ = done.send(());
            }
        }
        tracing::info!("stopped state worker");
    }
}
