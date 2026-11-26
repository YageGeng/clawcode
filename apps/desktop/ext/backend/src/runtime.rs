//! Independent network executor shared by desktop endpoints across local backend generations.

use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle, Runtime};
use tokio::task::JoinHandle;

/// Only an explicit inbound listener or outbound dial initializes this process-owned executor.
pub(super) static NETWORK: NetworkRuntime = NetworkRuntime(OnceLock::new());

pub(super) struct NetworkRuntime(OnceLock<Result<Runtime, String>>);

impl NetworkRuntime {
    /// Returns the network executor only if a dial or listener already started it.
    pub(super) fn existing(&self) -> Option<&Runtime> {
        self.0.get().and_then(|result| result.as_ref().ok())
    }

    /// Dispatches a dial or listener start independently of the JS engine and local kernel.
    pub(super) fn start<T: Send + 'static>(
        &self,
        label: &'static str,
        work: impl Future<Output = anyhow::Result<T>> + Send + 'static,
    ) -> impl Future<Output = Result<T, String>> {
        let task = self
            .0
            .get_or_init(|| {
                Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("clawcode-desktop")
                    .enable_all()
                    .build()
                    .map_err(|error| {
                        tracing::error!(
                            "failed to start desktop network runtime: {}",
                            error
                        );
                        error.to_string()
                    })
            })
            .as_ref()
            .map(|runtime| runtime.spawn(work))
            .map_err(Clone::clone);
        async move { finish_task(label, task?).await }
    }

    /// Detaches a settings action from its JS promise, reusing the engine runtime before the first dial.
    pub(super) fn controls<T: Send + 'static>(
        &self,
        label: &'static str,
        work: impl Future<Output = anyhow::Result<T>> + Send + 'static,
    ) -> impl Future<Output = Result<T, String>> {
        let task = match self.existing() {
            Some(network) => network.spawn(work),
            None => Handle::current().spawn(work),
        };
        finish_task(label, task)
    }
}

/// Converts a completed disk or network job into the same diagnostic at the NSH boundary.
pub(super) async fn finish_task<T>(
    label: &'static str,
    task: JoinHandle<anyhow::Result<T>>,
) -> Result<T, String> {
    task.await
        .map_err(anyhow::Error::from)
        .and_then(|result| result)
        .map_err(|error| {
            tracing::warn!("desktop {} failed: {:#}", label, error);
            format!("{error:#}")
        })
}
