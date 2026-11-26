//! Lifecycle of the production application and its generation-owned executor.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use acp::AcpServerFactory;
use app::ApplicationFactory;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use futures::{SinkExt, StreamExt};
use tokio::sync::watch;
use typed_builder::TypedBuilder;

use crate::hosting::HostBackend;

/// How long a generation waits for uninterruptible blocking tools after its kernel stops.
const RUNTIME_DRAIN: Duration = Duration::from_secs(5);

/// The local generation owns both its Tokio runtime and its worker lifecycle accounting.
struct KernelRuntime {
    runtime: tokio::runtime::Runtime,
    workers: Arc<AtomicUsize>,
}

impl KernelRuntime {
    /// Count workers before the OS starts them, including workers created for blocking jobs.
    fn new() -> std::io::Result<Self> {
        let workers = Arc::new(AtomicUsize::new(0));
        let starting = Arc::clone(&workers);
        let stopping = Arc::clone(&workers);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name_fn(move || {
                // A scheduled thread may first run after shutdown's waiting budget has expired.
                starting.fetch_add(1, Ordering::AcqRel);
                "clawcode-kernel".to_owned()
            })
            .on_thread_stop(move || {
                stopping.fetch_sub(1, Ordering::AcqRel);
            })
            .enable_all()
            .build()?;
        Ok(Self { runtime, workers })
    }

    /// Cancel detached Runs and confirm that all scheduled workers finished within a bounded wait.
    fn drain(self) -> Result<(), String> {
        let started = Instant::now();
        self.runtime.shutdown_timeout(RUNTIME_DRAIN);
        // Tokio returns early after a successful join, even if an OS thread creation failed
        // after requesting a name. A caller rescheduled late can still confirm every worker exited.
        if self.workers.load(Ordering::Acquire) == 0
            || started.elapsed() < RUNTIME_DRAIN
        {
            return Ok(());
        }
        let message = format!(
            "desktop kernel runtime drain timed out before worker shutdown could be confirmed ({RUNTIME_DRAIN:?})"
        );
        tracing::warn!("{}", message);
        Err(message)
    }
}

/// What the backend writes: its frames in order, then why it ended.
pub struct BackendOutput {
    /// Every frame the server writes; ends when the connection ends.
    pub frames: UnboundedReceiver<String>,
    /// Resolves once the local application has stopped: the error that ended the
    /// connection, or `None` for a normal end or a shutdown.
    pub ended: oneshot::Receiver<Option<String>>,
}

/// The running local application. Dropping this handle signals its task to shut down.
#[derive(TypedBuilder)]
pub struct BackendThread {
    pub(super) hosting: Arc<HostBackend>,
    input: UnboundedSender<String>,
    #[builder(default)]
    shutdown: Option<oneshot::Sender<()>>,
    /// The final drain result, shared by current and future shutdown waiters.
    stopped: watch::Receiver<Option<Result<(), String>>>,
    /// The same event for a blocking waiter: the app's quit, when no task runs.
    exited: mpsc::Receiver<()>,
}

impl BackendThread {
    /// Starts the local backend and resolves once the application is built,
    /// or with the reason it could not start.
    pub async fn start(
        config: config::ConfigHandle,
    ) -> Result<(BackendThread, BackendOutput), String> {
        let (input, incoming) = unbounded::<String>();
        let (outgoing, frames) = unbounded::<String>();
        let (ready_tx, ready_rx) =
            oneshot::channel::<Result<Arc<HostBackend>, String>>();
        let (ended_tx, ended_rx) = oneshot::channel::<Option<String>>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (stopped_tx, stopped) = watch::channel(None);
        let (exited_tx, exited_rx) = mpsc::sync_channel::<()>(1);
        let task = BackendTask::builder()
            .config(config)
            .incoming(incoming)
            .outgoing(outgoing)
            .shutdown(shutdown_rx)
            .build();
        // Detached Runs belong to this generation. Runtime shutdown cancels them even
        // when no ACP client remains to issue a session cancellation.
        std::thread::Builder::new()
            .name("clawcode-backend".to_owned())
            .spawn(move || {
                let (reason, drain) = match KernelRuntime::new() {
                    Ok(kernel) => {
                        let reason =
                            kernel.runtime.block_on(task.run(ready_tx));
                        (reason, kernel.drain())
                    }
                    Err(error) => {
                        tracing::error!(
                            "failed to create desktop kernel runtime: {}",
                            error
                        );
                        let _ = ready_tx.send(Err(error.to_string()));
                        (None, Err(error.to_string()))
                    }
                };
                let _ = ended_tx.send(drain.as_ref().err().cloned().or(reason));
                stopped_tx.send_replace(Some(drain));
                // Quit blocks on a standard channel after the engine stops polling promises.
                let _ = exited_tx.send(());
            })
            .map_err(|error| {
                tracing::error!(
                    "failed to spawn local backend thread: {}",
                    error
                );
                error.to_string()
            })?;
        let hosting = ready_rx.await.map_err(|error| {
            tracing::error!(
                "desktop backend task exited during startup: {}",
                error
            );
            format!("backend task exited during startup: {error}")
        })??;
        Ok((
            BackendThread::builder()
                .hosting(hosting)
                .input(input)
                .shutdown(Some(shutdown_tx))
                .stopped(stopped)
                .exited(exited_rx)
                .build(),
            BackendOutput {
                frames,
                ended: ended_rx,
            },
        ))
    }

    /// Sends one frame to the server; fails once the connection has ended.
    pub fn send(&self, frame: String) -> Result<(), String> {
        self.input
            .unbounded_send(frame)
            .map_err(|error| format!("the backend has stopped: {error}"))
    }

    /// Ends the connection, shuts the application down, and resolves once the
    /// runtime has drained, or returns its terminal failure to every waiter.
    pub fn shutdown(
        &mut self,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        self.request_shutdown();
        let mut stopped = self.stopped.clone();
        async move {
            loop {
                if let Some(result) = stopped.borrow_and_update().clone() {
                    return result;
                }
                stopped.changed().await.map_err(|error| {
                    tracing::error!("desktop backend exited without reporting its drain: {}", error);
                    "desktop backend exited without reporting its drain".to_owned()
                })?;
            }
        }
    }

    /// Ends the connection and blocks the calling thread until the backend
    /// has stopped or `timeout` passed; whether it stopped. For the
    /// app's quit, where no task runs again and the process exits next.
    pub fn shutdown_blocking(&mut self, timeout: Duration) -> bool {
        self.request_shutdown();
        match self.exited.recv_timeout(timeout) {
            // The sender closes after publishing the result, or on an unexpected thread exit.
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                matches!(*self.stopped.borrow(), Some(Ok(())))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    "desktop backend did not stop within {:?}",
                    timeout
                );
                false
            }
        }
    }

    /// Closes the input and asks the local application to shut down; later sends fail.
    fn request_shutdown(&mut self) {
        self.input.close_channel();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// What the local application task owns while it runs.
#[derive(TypedBuilder)]
struct BackendTask {
    config: config::ConfigHandle,
    incoming: UnboundedReceiver<String>,
    outgoing: UnboundedSender<String>,
    shutdown: oneshot::Receiver<()>,
}

impl BackendTask {
    /// Builds the application, serves the connection until it ends or a
    /// shutdown is asked for, then shuts the application down. Returns the
    /// error that ended the connection, if any.
    async fn run(
        self,
        ready: oneshot::Sender<Result<Arc<HostBackend>, String>>,
    ) -> Option<String> {
        let max_batch_size = self.config.current().app.recovery.max_batch_size;
        let application = match ApplicationFactory::new(self.config)
            .build()
            .await
        {
            Ok(application) => application,
            Err(error) => {
                tracing::error!("desktop backend failed to start: {}", error);
                let _ = ready.send(Err(error.to_string()));
                return None;
            }
        };
        let server = Arc::new(AcpServerFactory::new(
            Arc::clone(&application.kernel),
            max_batch_size,
        ));
        let hosting = Arc::new(HostBackend::new(&server));
        tracing::info!("desktop backend started");
        let _ = ready.send(Ok(Arc::clone(&hosting)));
        let incoming = self.incoming.map(Ok::<_, std::io::Error>);
        let outgoing = self.outgoing.sink_map_err(std::io::Error::other);
        // A connection that ends on its own also shuts the application down,
        // so its SQLite lease and pending state syncs do not outlive it.
        let reason = tokio::select! {
            _ = self.shutdown => None,
            () = application.kernel.wait_for_shutdown() => None,
            result = server.serve_lines(outgoing, incoming) => result.err().map(|error| {
                tracing::error!("desktop backend connection failed: {}", error);
                error.to_string()
            }),
        };
        // Release every remotely served connection before flushing the shared kernel.
        hosting.shutdown().await;
        if let Err(error) = application.shutdown().await {
            tracing::error!("desktop backend shutdown failed: {}", error);
        }
        tracing::info!("desktop backend stopped");
        reason
    }
}
