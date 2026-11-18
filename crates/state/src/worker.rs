//! Background catch-up of Sessions after each durable store sync.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use protocol::SessionId;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::{StateError, StateRuntime};

type Flush = oneshot::Sender<Result<(), StateError>>;

/// A flush is a barrier for preceding notifications, including failed writes.
enum WorkerMessage {
    Synced(SessionId, PathBuf),
    Flush(Flush),
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
        let task = tokio::spawn(
            WorkerLoop {
                runtime,
                receiver,
                failed: BTreeMap::new(),
            }
            .run(),
        );
        (Arc::new(Self { sender }), task)
    }

    /// Confirms that every preceding sync reached SQLite, or identifies a failed Session.
    pub async fn flush(&self) -> Result<(), StateError> {
        let (done, wait) = oneshot::channel();
        self.sender
            .send(WorkerMessage::Flush(done))
            .map_err(|_closed| StateError::WorkerStopped)?;
        wait.await.map_err(|_closed| StateError::WorkerStopped)?
    }
}

impl store::SyncObserver for StateWorker {
    /// Enqueues a catch-up without blocking the syncing store.
    fn synced(&self, session_id: &SessionId, path: &Path) {
        if self
            .sender
            .send(WorkerMessage::Synced(
                session_id.clone(),
                path.to_path_buf(),
            ))
            .is_err()
        {
            tracing::warn!(
                "state worker stopped before projecting session {}",
                session_id
            );
        }
    }
}

/// A transient failure gets a delayed retry; other failures wait for a new sync or flush.
struct FailedSync {
    path: PathBuf,
    error: StateError,
    retry: Option<(Instant, Duration)>,
}

/// Receiving side of the worker channel.
struct WorkerLoop {
    runtime: Arc<StateRuntime>,
    receiver: mpsc::UnboundedReceiver<WorkerMessage>,
    failed: BTreeMap<SessionId, FailedSync>,
}

impl WorkerLoop {
    /// Drains bounded batches up to the next flush, including transient work that is due.
    async fn run(mut self) {
        loop {
            let deadline = self
                .failed
                .values()
                .filter_map(|item| item.retry.map(|r| r.0))
                .min();
            let first = match deadline {
                Some(at) => tokio::select! {
                    message = self.receiver.recv() => message,
                    () = tokio::time::sleep_until(at) => None,
                },
                None => self.receiver.recv().await,
            };
            let closed = first.is_none()
                && self.receiver.is_closed()
                && self.receiver.is_empty();
            let mut pending = BTreeMap::new();
            let mut flush = None;
            if let Some(first) = first {
                for message in std::iter::once(first)
                    .chain(std::iter::from_fn(|| self.receiver.try_recv().ok()))
                    .take(128)
                {
                    match message {
                        WorkerMessage::Synced(id, path) => {
                            pending.insert(id, path);
                        }
                        WorkerMessage::Flush(done) => {
                            flush = Some(done);
                            break;
                        }
                    }
                }
            }
            let now = Instant::now();
            for (id, failure) in &self.failed {
                if flush.is_some()
                    || closed
                    || failure.retry.is_some_and(|(at, _)| at <= now)
                {
                    pending
                        .entry(id.clone())
                        .or_insert_with(|| failure.path.clone());
                }
            }
            for (id, path) in pending {
                let previous = self.failed.remove(&id);
                if let Err(error) = self.project(&id, &path).await {
                    let retry = error.is_busy().then(|| {
                        let delay = previous
                            .and_then(|p| p.retry)
                            .map_or(Duration::from_millis(100), |(_, d)| {
                                (d * 2).min(Duration::from_secs(5))
                            });
                        (Instant::now() + delay, delay)
                    });
                    tracing::warn!(
                        "failed to project session {} after sync: {}",
                        id,
                        error
                    );
                    self.failed.insert(id, FailedSync { path, error, retry });
                }
            }
            if let Some(done) = flush {
                let result = self.failed.first_key_value().map_or(
                    Ok(()),
                    |(id, failure)| {
                        Err(StateError::WorkerProjection {
                            count: self.failed.len(),
                            session_id: id.clone(),
                            reason: failure.error.to_string(),
                        })
                    },
                );
                let _ = done.send(result);
            }
            if closed {
                break;
            }
        }
        tracing::info!(
            "stopped state worker with {} outstanding sessions",
            self.failed.len()
        );
    }

    /// A queued sync may race deletion or a move; only remove rows for the vanished path.
    async fn project(
        &self,
        id: &SessionId,
        path: &Path,
    ) -> Result<(), StateError> {
        match self.runtime.refresh(id, path).await {
            Err(StateError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                let path = std::path::absolute(path)?;
                self.runtime.delete_session_at(id, Some(&path)).await
            }
            result => result,
        }
    }
}
