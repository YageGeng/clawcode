use std::sync::Mutex;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::runtime::KernelError;

/// Background tasks owned by one live Session and the signal that stops them.
#[derive(Default)]
pub(in crate::runtime) struct SessionBackgroundWork {
    pub(in crate::runtime) shutdown: CancellationToken,
    tasks: Mutex<JoinSet<()>>,
}

impl SessionBackgroundWork {
    /// Tracks one background task and reaps settled ones without blocking;
    /// refused once the owning Session started shutting down.
    pub(in crate::runtime) fn spawn(
        &self,
        task: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), KernelError> {
        let mut tasks = self.tasks.lock()?;
        if self.shutdown.is_cancelled() {
            tracing::warn!("refused background task for a closing session");
            return Err(KernelError::SessionClosing);
        }
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                tracing::warn!("session background task failed: {}", error);
            }
        }
        tasks.spawn(task);
        Ok(())
    }

    /// Waits outside the synchronous lock after all child work has been cancelled.
    pub(in crate::runtime) async fn join(&self) -> Result<(), KernelError> {
        let mut tasks = std::mem::take(&mut *self.tasks.lock()?);
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::warn!(
                    "session background task failed during shutdown: {}",
                    error
                );
            }
        }
        Ok(())
    }
}
