//! The production `Application` on its own thread, serving one ACP
//! connection whose JSON-RPC messages travel as text frames.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use acp::AcpServerFactory;
use app::ApplicationFactory;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use futures::{SinkExt, StreamExt};
use typed_builder::TypedBuilder;

/// What the backend writes: its frames in order, then why it ended.
pub struct BackendOutput {
    /// Every frame the server writes; ends when the connection ends.
    pub frames: UnboundedReceiver<String>,
    /// Resolves once the thread has stopped: the error that ended the
    /// connection, or `None` for a normal end or a shutdown.
    pub ended: oneshot::Receiver<Option<String>>,
}

/// How long the thread's runtime waits for blocking work (a tool batch, a
/// search) once the application has shut down, before it abandons it.
const RUNTIME_DRAIN: Duration = Duration::from_secs(5);

/// A running backend thread; dropping it closes the connection, which stops the thread.
#[derive(TypedBuilder)]
pub struct BackendThread {
    input: UnboundedSender<String>,
    shutdown: Option<oneshot::Sender<()>>,
    /// Resolves for async waiters once the thread has stopped.
    stopped: Option<oneshot::Receiver<()>>,
    /// The same event for a blocking waiter: the app's quit, when no task runs.
    exited: Option<mpsc::Receiver<()>>,
}

impl BackendThread {
    /// Starts the backend thread and resolves once the application is built,
    /// or with the reason it could not start.
    pub async fn start(
        config: config::ConfigHandle,
    ) -> Result<(BackendThread, BackendOutput), String> {
        let (input, incoming) = unbounded::<String>();
        let (outgoing, frames) = unbounded::<String>();
        let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
        let (ended_tx, ended_rx) = oneshot::channel::<Option<String>>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (stopped_tx, stopped_rx) = oneshot::channel::<()>();
        let (exited_tx, exited_rx) = mpsc::sync_channel::<()>(1);
        let task = BackendTask::builder()
            .config(config)
            .incoming(incoming)
            .outgoing(outgoing)
            .shutdown(shutdown_rx)
            .build();
        std::thread::Builder::new()
            .name("clawcode-backend".to_owned())
            .spawn(move || {
                let reason = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        let reason = runtime.block_on(task.run(ready_tx));
                        // A plain drop would wait without bound for blocking work.
                        runtime.shutdown_timeout(RUNTIME_DRAIN);
                        reason
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        None
                    }
                };
                let _ = ended_tx.send(reason);
                // Sent last: the thread has nothing left to release.
                let _ = stopped_tx.send(());
                let _ = exited_tx.send(());
            })
            .map_err(|error| error.to_string())?;
        ready_rx.await.map_err(|error| {
            format!("backend thread exited during startup: {error}")
        })??;
        Ok((
            BackendThread::builder()
                .input(input)
                .shutdown(Some(shutdown_tx))
                .stopped(Some(stopped_rx))
                .exited(Some(exited_rx))
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
    /// thread has stopped. Later sends fail. The caller bounds the wait.
    pub fn shutdown(&mut self) -> impl Future<Output = ()> + use<> {
        self.request_shutdown();
        let stopped = self.stopped.take();
        async move {
            if let Some(stopped) = stopped {
                // A dropped sender also means the thread finished.
                let _ = stopped.await;
            }
        }
    }

    /// Ends the connection and blocks the calling thread until the backend
    /// thread has stopped or `timeout` passed; whether it stopped. For the
    /// app's quit, where no task runs again and the process exits next.
    pub fn shutdown_blocking(&mut self, timeout: Duration) -> bool {
        self.request_shutdown();
        let Some(exited) = self.exited.take() else {
            return true;
        };
        match exited.recv_timeout(timeout) {
            // A dropped sender also means the thread finished.
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    "desktop backend did not stop within {:?}",
                    timeout
                );
                false
            }
        }
    }

    /// Closes the input and asks the thread to shut down; later sends fail.
    fn request_shutdown(&mut self) {
        self.input.close_channel();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// What the backend thread owns while it runs.
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
        ready: oneshot::Sender<Result<(), String>>,
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
        let server = AcpServerFactory::new(
            Arc::clone(&application.kernel),
            max_batch_size,
        );
        tracing::info!("desktop backend started");
        let _ = ready.send(Ok(()));
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
        if let Err(error) = application.shutdown().await {
            tracing::error!("desktop backend shutdown failed: {}", error);
        }
        tracing::info!("desktop backend stopped");
        reason
    }
}
