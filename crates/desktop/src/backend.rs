//! In-process agent backend: the production `Application` behind an ACP v2 client connection.
//!
//! The backend lives on its own thread with a tokio runtime. The UI holds an
//! [`AcpClient`], whose requests are spawned onto that runtime, and reads
//! session notifications from an unbounded [`BackendEvent`] stream.

use std::path::Path;
use std::sync::Arc;

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::{ConnectionTo, schema as wire};
use agent_client_protocol::{
    Agent, Client, ConnectTo, JsonRpcMessage, JsonRpcRequest, UntypedMessage,
};
use app::ApplicationFactory;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use protocol::{
    AcpExtensionMethod, PendingMessages, QueueId, SessionRuntimeSnapshot,
};

/// ACP session identifier used throughout the desktop client.
pub type SessionId = wire::SessionId;

/// Something the backend reports to the UI.
#[derive(Debug)]
pub enum BackendEvent {
    /// One `session/update` notification, in arrival order.
    Update(Box<wire::UpdateSessionNotification>),
    /// The ACP connection ended; carries the error when it ended with one.
    Stopped(Option<String>),
}

/// Why a backend operation failed.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// The application or the ACP connection could not be started.
    #[error("{0}")]
    Startup(String),
    /// The agent rejected a request or answered with an unexpected shape.
    #[error("{0}")]
    Request(String),
    /// The backend runtime is gone.
    #[error("the backend has stopped")]
    Stopped,
    /// The agent cancelled the request (JSON-RPC `-32800`), for example a
    /// queued prompt that was removed or whose run was stopped.
    #[error("the request was cancelled")]
    Cancelled,
}

impl From<agent_client_protocol::Error> for BackendError {
    /// Keeps cancellation distinguishable from real failures.
    fn from(error: agent_client_protocol::Error) -> Self {
        if error.code == agent_client_protocol::ErrorCode::REQUEST_CANCELLED {
            BackendError::Cancelled
        } else {
            BackendError::Request(error.to_string())
        }
    }
}

/// Handle to the in-process ACP connection; cheap to clone and usable from any executor.
#[derive(Clone)]
pub struct AcpClient {
    connection: ConnectionTo<Agent>,
    runtime: tokio::runtime::Handle,
}

/// Keeps the ACP connection open until [`AcpConnection::close`] or drop.
pub struct AcpConnection {
    shutdown: oneshot::Sender<()>,
    /// The connection driver; `None` once it finished and was awaited.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl AcpConnection {
    /// Resolves when the connection ends on its own, for example on a protocol error.
    pub async fn ended(&mut self) {
        if let Some(task) = &mut self.task {
            if let Err(error) = task.await {
                tracing::warn!("desktop ACP connection task failed: {}", error);
            }
            // A finished JoinHandle must not be polled again.
            self.task = None;
        }
    }

    /// Ends the connection and waits for its driver task to finish.
    pub async fn close(self) {
        let AcpConnection { shutdown, task } = self;
        // The driver may already have stopped on its own; either way it is finishing now.
        let _ = shutdown.send(());
        if let Some(task) = task
            && let Err(error) = task.await
        {
            tracing::warn!("desktop ACP connection task failed: {}", error);
        }
    }
}

/// Untyped request for one product extension method, answered with raw JSON.
#[derive(Debug, Clone)]
struct ExtRequest {
    method: &'static str,
    params: serde_json::Value,
}

impl JsonRpcMessage for ExtRequest {
    /// Product extension methods share one namespace prefix.
    fn matches_method(method: &str) -> bool {
        method.starts_with("_clawcode/")
    }

    /// Returns the extension method name.
    fn method(&self) -> &str {
        self.method
    }

    /// Serializes the parameters through the untyped ACP envelope.
    fn to_untyped_message(
        &self,
    ) -> Result<UntypedMessage, agent_client_protocol::Error> {
        UntypedMessage::new(self.method, &self.params)
    }

    /// The desktop only sends these requests; it never receives them.
    fn parse_message(
        _method: &str,
        _params: &serde_json::Value,
    ) -> Result<Self, agent_client_protocol::Error> {
        Err(agent_client_protocol::Error::method_not_found())
    }
}

impl JsonRpcRequest for ExtRequest {
    type Response = serde_json::Value;
}

impl AcpClient {
    /// Connects to an in-process ACP agent and completes `initialize`; call inside a tokio runtime.
    ///
    /// Every `session/update` is forwarded to `events`; when the connection
    /// ends, one [`BackendEvent::Stopped`] follows.
    pub async fn connect(
        component: impl ConnectTo<Client> + 'static,
        events: UnboundedSender<BackendEvent>,
    ) -> Result<(AcpClient, AcpConnection), BackendError> {
        let (ready_tx, ready_rx) =
            oneshot::channel::<Result<ConnectionTo<Agent>, String>>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let updates = events.clone();
        let task = tokio::spawn(async move {
            let result = Client
                .v2()
                .name("clawcode-desktop")
                .on_receive_notification(
                    async move |notification: wire::UpdateSessionNotification, _connection| {
                        // A closed receiver means the UI is gone; nothing is left to notify.
                        let _ = updates.unbounded_send(BackendEvent::Update(Box::new(notification)));
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(component, async move |connection| {
                    let initialize = connection
                        .send_request(wire::InitializeRequest::new(
                            ProtocolVersion::V2,
                            wire::Implementation::new(
                                "clawcode-desktop",
                                env!("CARGO_PKG_VERSION"),
                            ),
                        ))
                        .block_task()
                        .await;
                    if let Err(error) = initialize {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return Err(error);
                    }
                    let _ = ready_tx.send(Ok(connection.clone()));
                    // Either an explicit close or a dropped `AcpConnection` ends the connection.
                    let _ = shutdown_rx.await;
                    Ok(())
                })
                .await;
            let reason = result.err().map(|error| error.to_string());
            match &reason {
                Some(error) => tracing::warn!(
                    "desktop ACP connection ended with an error: {}",
                    error
                ),
                None => tracing::info!("desktop ACP connection closed"),
            }
            let _ = events.unbounded_send(BackendEvent::Stopped(reason));
        });
        let connection = ready_rx
            .await
            .map_err(|error| {
                BackendError::Startup(format!(
                    "ACP connection ended during initialize: {error}"
                ))
            })?
            .map_err(BackendError::Startup)?;
        Ok((
            AcpClient {
                connection,
                runtime: tokio::runtime::Handle::current(),
            },
            AcpConnection {
                shutdown: shutdown_tx,
                task: Some(task),
            },
        ))
    }

    /// Sends one request on the backend runtime and waits for its response.
    async fn request<R>(&self, request: R) -> Result<R::Response, BackendError>
    where
        R: JsonRpcRequest + Send + 'static,
        R::Response: Send + 'static,
    {
        let connection = self.connection.clone();
        self.runtime
            .spawn(async move {
                connection.send_request(request).block_task().await
            })
            .await
            .map_err(|error| {
                BackendError::Request(format!("backend task failed: {error}"))
            })?
            .map_err(BackendError::from)
    }

    /// Lists every persisted session.
    pub async fn list_sessions(
        &self,
    ) -> Result<Vec<wire::SessionInfo>, BackendError> {
        Ok(self
            .request(wire::ListSessionsRequest::new())
            .await?
            .sessions)
    }

    /// Creates a session whose working directory is `cwd`.
    pub async fn new_session(
        &self,
        cwd: &Path,
    ) -> Result<SessionId, BackendError> {
        Ok(self
            .request(wire::NewSessionRequest::new(cwd.to_path_buf()))
            .await?
            .session_id)
    }

    /// Attaches to a session and replays its whole retained transcript as updates.
    pub async fn resume_with_replay(
        &self,
        id: &SessionId,
        cwd: &Path,
    ) -> Result<(), BackendError> {
        self.request(
            wire::ResumeSessionRequest::new(id.clone(), cwd.to_path_buf())
                .replay_from(wire::ReplayFrom::Start(
                    wire::ReplayFromStart::new(),
                )),
        )
        .await?;
        Ok(())
    }

    /// Releases a session's live resources in the backend.
    pub async fn close_session(
        &self,
        id: &SessionId,
    ) -> Result<(), BackendError> {
        self.request(wire::CloseSessionRequest::new(id.clone()))
            .await?;
        Ok(())
    }

    /// Deletes a session and its history.
    pub async fn delete_session(
        &self,
        id: &SessionId,
    ) -> Result<(), BackendError> {
        self.request(wire::DeleteSessionRequest::new(id.clone()))
            .await?;
        Ok(())
    }

    /// Renames a session.
    pub async fn set_title(
        &self,
        id: &SessionId,
        title: &str,
    ) -> Result<(), BackendError> {
        self.request(wire::SetSessionTitleRequest::new(id.clone(), title))
            .await?;
        Ok(())
    }

    /// Reports whether the session currently owns an active run.
    pub async fn session_running(
        &self,
        id: &SessionId,
    ) -> Result<bool, BackendError> {
        let snapshot = self
            .request(ExtRequest {
                method: AcpExtensionMethod::SessionRuntime.as_str(),
                params: serde_json::json!({ "sessionId": id }),
            })
            .await?;
        serde_json::from_value::<SessionRuntimeSnapshot>(snapshot)
            .map(|snapshot| snapshot.running)
            .map_err(|error| {
                BackendError::Request(format!(
                    "invalid session runtime snapshot: {error}"
                ))
            })
    }

    /// Sends one prompt made of content blocks; the reply arrives as session updates.
    pub async fn prompt(
        &self,
        id: &SessionId,
        blocks: Vec<wire::ContentBlock>,
    ) -> Result<(), BackendError> {
        self.request(wire::PromptRequest::new(id.clone(), blocks))
            .await?;
        Ok(())
    }

    /// Sends one product extension request and returns its raw JSON answer.
    async fn extension(
        &self,
        method: AcpExtensionMethod,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, BackendError> {
        self.request(ExtRequest {
            method: method.as_str(),
            params,
        })
        .await
    }

    /// Reads the session's queued steering and follow-up messages.
    pub async fn pending_messages(
        &self,
        id: &SessionId,
    ) -> Result<PendingMessages, BackendError> {
        let value = self
            .extension(
                AcpExtensionMethod::PendingMessages,
                serde_json::json!({ "sessionId": id }),
            )
            .await?;
        serde_json::from_value(value).map_err(|error| {
            BackendError::Request(format!("invalid pending messages: {error}"))
        })
    }

    /// Moves a queued message to the steering lane; returns whether it moved.
    pub async fn promote_pending(
        &self,
        id: &SessionId,
        queue_id: &QueueId,
    ) -> Result<bool, BackendError> {
        let value = self
            .extension(
                AcpExtensionMethod::PendingMessagePromote,
                serde_json::json!({ "sessionId": id, "queueId": queue_id }),
            )
            .await?;
        value
            .get("promoted")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| {
                BackendError::Request(format!(
                    "invalid promote response: {value}"
                ))
            })
    }

    /// Removes a queued message.
    pub async fn remove_pending(
        &self,
        id: &SessionId,
        queue_id: &QueueId,
    ) -> Result<(), BackendError> {
        self.extension(
            AcpExtensionMethod::PendingMessageRemove,
            serde_json::json!({ "sessionId": id, "queueId": queue_id }),
        )
        .await?;
        Ok(())
    }

    /// Asks the agent to stop the session's current run.
    pub fn cancel(&self, id: &SessionId) -> Result<(), BackendError> {
        self.connection
            .send_notification(wire::CancelSessionNotification::new(id.clone()))
            .map_err(|error| BackendError::Request(error.to_string()))
    }
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
