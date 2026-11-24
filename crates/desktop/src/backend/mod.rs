//! In-process agent backend: the production `Application` behind an ACP v2 client connection.
//!
//! The backend lives on its own thread with a tokio runtime. The UI holds an
//! [`AcpClient`], whose requests are spawned onto that runtime, and reads
//! session notifications from an unbounded [`BackendEvent`] stream.

mod client;
mod error;
mod requests;
mod sign_in;

use std::sync::Arc;

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::v2::schema as wire;
use app::ApplicationFactory;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;

pub use client::{AcpClient, AcpConnection};
pub use error::{BackendError, StartError};
pub use requests::{AgentRequest, RequestId};
pub use sign_in::{SignIn, SignInOutcome, SignInPrompt};

/// ACP session identifier used throughout the desktop client.
pub type SessionId = wire::SessionId;

/// Something the backend reports to the UI.
#[derive(Debug)]
pub enum BackendEvent {
    /// One `session/update` notification, in arrival order.
    Update(Box<wire::UpdateSessionNotification>),
    /// The agent asks the user something and waits for the answer.
    Request(Box<AgentRequest>),
    /// A sign-in wants its device code entered; it has no session.
    SignIn(RequestId, SignInPrompt),
    /// The agent withdrew a request: it was answered elsewhere or its run ended.
    RequestCancelled(RequestId),
    /// The ACP connection ended; carries the error when it ended with one.
    Stopped(Option<String>),
}

/// Identity of one asynchronous question: the asking call and its position.
pub use protocol::QuestionKey;

/// A session just created or resumed, with its current configuration options.
#[derive(Debug)]
pub struct SessionStart {
    /// The session's identifier.
    pub id: SessionId,
    /// Model, thinking-level, and mode options as the agent reports them.
    pub config_options: Vec<wire::SessionConfigOption>,
}

/// The running backend thread: production `Application` plus its in-process client.
pub struct Backend {
    client: AcpClient,
    shutdown: Option<oneshot::Sender<()>>,
    stopped: oneshot::Receiver<()>,
}

impl Backend {
    /// Starts the backend thread and resolves once the ACP connection is initialized or failed.
    pub async fn start(
        config: config::ConfigHandle,
    ) -> Result<(Backend, UnboundedReceiver<BackendEvent>), BackendError> {
        let (events_tx, events_rx) = unbounded();
        let (ready_tx, ready_rx) =
            oneshot::channel::<Result<AcpClient, BackendError>>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (stopped_tx, stopped_rx) = oneshot::channel::<()>();
        std::thread::Builder::new()
            .name("clawcode-backend".to_string())
            .spawn(move || {
                match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(BackendTask::run(
                        BackendTask {
                            config,
                            events: events_tx,
                            shutdown: shutdown_rx,
                        },
                        ready_tx,
                    )),
                    Err(error) => {
                        let _ = ready_tx.send(Err(BackendError::Startup(
                            error.to_string(),
                        )));
                    }
                }
                let _ = stopped_tx.send(());
            })
            .map_err(|error| BackendError::Startup(error.to_string()))?;
        let client = ready_rx.await.map_err(|error| {
            BackendError::Startup(format!(
                "backend thread exited during startup: {error}"
            ))
        })??;
        Ok((
            Backend {
                client,
                shutdown: Some(shutdown_tx),
                stopped: stopped_rx,
            },
            events_rx,
        ))
    }

    /// Returns the client connected to this backend.
    pub fn client(&self) -> &AcpClient {
        &self.client
    }

    /// Closes the connection, shuts the application down, and waits for the thread to finish.
    ///
    /// The caller bounds the wait; the returned future does not time out itself.
    pub fn shutdown(mut self) -> impl Future<Output = ()> + Send {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        async move {
            // A dropped sender also means the thread finished.
            let _ = self.stopped.await;
        }
    }
}

/// What the backend thread owns while it runs.
struct BackendTask {
    config: config::ConfigHandle,
    events: UnboundedSender<BackendEvent>,
    shutdown: oneshot::Receiver<()>,
}

impl BackendTask {
    /// Builds the application, connects the client, serves until asked to stop, then shuts down.
    async fn run(
        self,
        ready: oneshot::Sender<Result<AcpClient, BackendError>>,
    ) {
        let max_batch_size = self.config.current().app.recovery.max_batch_size;
        let application = match ApplicationFactory::new(self.config)
            .build()
            .await
        {
            Ok(application) => application,
            Err(error) => {
                tracing::error!("desktop backend failed to start: {}", error);
                let _ =
                    ready.send(Err(BackendError::Startup(error.to_string())));
                return;
            }
        };
        let server = AcpServerFactory::new(
            Arc::clone(&application.kernel),
            max_batch_size,
        );
        match AcpClient::connect(
            server.component(AcpTransportKind::InProcess),
            self.events,
        )
        .await
        {
            Ok((client, mut connection)) => {
                tracing::info!("desktop backend connected");
                let _ = ready.send(Ok(client));
                // A connection that ends on its own also shuts the application down,
                // so its SQLite lease and pending state syncs do not outlive it.
                tokio::select! {
                    _ = self.shutdown => {}
                    () = application.kernel.wait_for_shutdown() => {}
                    () = connection.ended() => {}
                }
                connection.close().await;
            }
            Err(error) => {
                tracing::error!("desktop backend failed to connect: {}", error);
                let _ = ready.send(Err(error));
            }
        }
        if let Err(error) = application.shutdown().await {
            tracing::error!("desktop backend shutdown failed: {}", error);
        }
        tracing::info!("desktop backend stopped");
    }
}
