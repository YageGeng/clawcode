//! Kernel-owned MCP Host composition without client filesystem callbacks.

mod host;
pub(super) use host::SessionMcpHost;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ::tools::ToolRegistry;
use protocol::{
    AgentEventPayload, McpElicitationAction, McpElicitationRequest,
    McpElicitationResponseRequest, McpElicitationResult,
    McpElicitationSnapshot, SessionId, TimestampMs, TurnId,
};
use tokio::sync::{Mutex as AsyncMutex, broadcast};

use super::{Kernel, KernelError, PendingMcpElicitation, Session};

// Recreated Session runtimes must outrank snapshots retained by an attached WebUI.
static MCP_UI_REVISION: AtomicU64 = AtomicU64::new(1);

/// Represents an absent or fully initialized Session-scoped MCP capability.
pub(super) enum SessionMcpRuntime {
    Disabled,
    Enabled(SessionMcpState),
}

/// Determines whether construction opens transports or borrows its owner's connection.
pub(super) enum SessionMcpSource<'a> {
    Connect,
    Share(&'a SessionMcpRuntime),
}

/// Only the transport owner may reconnect or shut down the shared MCP session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum McpLease {
    Owned,
    Shared,
}

/// MCP capability resolved for one Session before its runtime exists.
pub(super) struct AcquiredSessionMcp {
    pub(super) connection:
        Option<(Arc<::mcp::McpSession>, Arc<SessionMcpHost>)>,
    pub(super) tools: ToolRegistry,
    pub(super) lease: McpLease,
}

impl AcquiredSessionMcp {
    /// Returns the connection a failed construction must close; borrowed ones stay open.
    pub(super) fn owned_session(&self) -> Option<Arc<::mcp::McpSession>> {
        self.connection
            .as_ref()
            .filter(|_connection| self.lease == McpLease::Owned)
            .map(|(session, _host)| Arc::clone(session))
    }
}

impl SessionMcpSource<'_> {
    /// Opens owned transports or borrows the owner's connection for a new Session.
    pub(super) async fn acquire(
        self,
        kernel: &Kernel,
        session_id: &SessionId,
        cwd: &Path,
    ) -> Result<AcquiredSessionMcp, KernelError> {
        match self {
            Self::Share(owner) => Ok(match owner.state() {
                Some(state) => AcquiredSessionMcp {
                    connection: Some((
                        Arc::clone(&state.session),
                        Arc::clone(&state.host),
                    )),
                    tools: state.session.tool_registry().inspect_err(
                        |error| {
                            tracing::error!(
                                "failed shared MCP tool catalog read for session {}: {}",
                                session_id,
                                error
                            );
                        },
                    )?,
                    lease: McpLease::Shared,
                },
                None => AcquiredSessionMcp {
                    connection: None,
                    tools: ToolRegistry::default(),
                    lease: McpLease::Shared,
                },
            }),
            Self::Connect => {
                let Some(factory) = &kernel.mcp_factory else {
                    return Ok(AcquiredSessionMcp {
                        connection: None,
                        tools: ToolRegistry::default(),
                        lease: McpLease::Owned,
                    });
                };
                let host = Arc::new(SessionMcpHost::new(cwd.to_path_buf()));
                tracing::info!(
                    "started MCP Session creation for session {}",
                    session_id
                );
                let session = match factory
                    .create(
                        ::mcp::McpSessionRequest::builder()
                            .session_id(session_id.clone())
                            .cwd(cwd.to_path_buf())
                            .host(Arc::clone(&host) as Arc<dyn ::mcp::McpHost>)
                            .shutdown(kernel.shutdown.child_token())
                            .build(),
                    )
                    .await
                {
                    Ok(session) => {
                        tracing::info!(
                            "completed MCP Session creation for session {}",
                            session_id
                        );
                        Arc::new(session)
                    }
                    Err(error) => {
                        tracing::error!(
                            "failed MCP Session creation for session {}: {}",
                            session_id,
                            error
                        );
                        return Err(error.into());
                    }
                };
                let tools = match session.tool_registry() {
                    Ok(tools) => tools,
                    Err(error) => {
                        tracing::error!(
                            "failed MCP tool catalog construction for session {}: {}",
                            session_id,
                            error
                        );
                        // MCP was already created, so a catalog failure must not
                        // leave its background transports alive.
                        if let Err(shutdown_error) = session.shutdown().await {
                            tracing::warn!(
                                "failed to shut down MCP after catalog construction failed: {}",
                                shutdown_error
                            );
                        }
                        return Err(error.into());
                    }
                };
                Ok(AcquiredSessionMcp {
                    connection: Some((session, host)),
                    tools,
                    lease: McpLease::Owned,
                })
            }
        }
    }
}

/// Groups the MCP session, projection task, and pending elicitations.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct SessionMcpState {
    pub(super) session: Arc<::mcp::McpSession>,
    pub(super) host: Arc<SessionMcpHost>,
    session_id: SessionId,
    lease: McpLease,
    #[builder(default)]
    projection: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
    elicitations: Mutex<HashMap<String, PendingMcpElicitation>>,
    #[builder(default = Mutex::new(McpProjectionNotifications::default()))]
    notifications: Mutex<McpProjectionNotifications>,
}

/// Serializes UI invalidations without storing a second catalog or changing resource cursors.
pub(super) struct McpProjectionNotifications {
    revision: u64,
    sender: broadcast::Sender<::mcp::McpSessionEvent>,
}

impl Default for McpProjectionNotifications {
    /// Starts a connection-independent UI revision stream for this Session runtime.
    fn default() -> Self {
        Self {
            revision: MCP_UI_REVISION.fetch_add(1, Ordering::Relaxed),
            sender: broadcast::channel(128).0,
        }
    }
}

impl SessionMcpRuntime {
    /// Converts an optional MCP Session into the explicit capability state.
    pub(super) fn new(
        session: Option<(Arc<::mcp::McpSession>, Arc<SessionMcpHost>)>,
        session_id: SessionId,
        lease: McpLease,
    ) -> Self {
        session.map_or(Self::Disabled, |(session, host)| {
            Self::Enabled(
                SessionMcpState::builder()
                    .session(session)
                    .host(host)
                    .session_id(session_id)
                    .lease(lease)
                    .projection(AsyncMutex::new(None))
                    .elicitations(Mutex::new(HashMap::new()))
                    .build(),
            )
        })
    }

    /// Returns the enabled MCP Session handle when configured.
    pub(super) fn as_ref(&self) -> Option<&Arc<::mcp::McpSession>> {
        match self {
            Self::Disabled => None,
            Self::Enabled(state) => Some(&state.session),
        }
    }

    /// Returns enabled MCP state for projection and elicitation operations.
    pub(super) fn state(&self) -> Option<&SessionMcpState> {
        match self {
            Self::Disabled => None,
            Self::Enabled(state) => Some(state),
        }
    }

    /// Rejects transport management from a shared child's view of the connection.
    pub(super) fn ensure_owned(
        &self,
        session_id: &SessionId,
    ) -> Result<(), KernelError> {
        if self
            .state()
            .is_some_and(|state| state.lease == McpLease::Shared)
        {
            tracing::warn!(
                "refused MCP management from shared session {}",
                session_id
            );
            return Err(KernelError::SharedMcpManagement(session_id.clone()));
        }
        Ok(())
    }

    /// Shuts down the MCP Session and always joins its projection task.
    pub(super) async fn shutdown(&self) -> Result<(), KernelError> {
        let Some(state) = self.state() else {
            return Ok(());
        };
        tracing::info!("started MCP shutdown for session {}", state.session_id);
        state.host.detach(&state.session_id);
        let shutdown_result = match state.lease {
            McpLease::Owned => state.session.shutdown().await,
            McpLease::Shared => Ok(()),
        };
        let projection_result =
            if let Some(task) = state.projection.lock().await.take() {
                if state.lease == McpLease::Shared {
                    task.abort();
                }
                match task.await {
                    Ok(()) => Ok(()),
                    Err(error)
                        if state.lease == McpLease::Shared
                            && error.is_cancelled() =>
                    {
                        Ok(())
                    }
                    Err(error) => Err(KernelError::Protocol(format!(
                        "MCP Tool projection task failed: {error}"
                    ))),
                }
            } else {
                Ok(())
            };
        let result: Result<(), KernelError> = match (
            shutdown_result,
            projection_result,
        ) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error.into()),
            (Err(error), Err(projection_error)) => {
                // Keep the MCP shutdown failure primary while preserving the
                // independently attempted projection cleanup in diagnostics.
                tracing::warn!(
                    "MCP projection cleanup also failed after shutdown error: {}",
                    projection_error
                );
                Err(error.into())
            }
        };
        match &result {
            Ok(()) => tracing::info!(
                "completed MCP shutdown for session {}",
                state.session_id
            ),
            Err(error) => tracing::error!(
                "failed MCP shutdown for session {}: {}",
                state.session_id,
                error
            ),
        }
        result
    }
}

impl Kernel {
    /// Returns one Session's complete transient MCP elicitation state.
    pub fn mcp_elicitations(
        &self,
        session_id: &SessionId,
    ) -> Result<McpElicitationSnapshot, KernelError> {
        let session = self.session(session_id)?;
        let Some(state) = session.mcp.state() else {
            return Ok(McpElicitationSnapshot {
                session_id: session_id.clone(),
                requests: Vec::new(),
            });
        };
        let mut requests = state
            .elicitations
            .lock()?
            .values()
            .map(|pending| pending.request.clone())
            .collect::<Vec<_>>();
        requests.sort_by(|left, right| left.request_id.cmp(&right.request_id));
        Ok(McpElicitationSnapshot {
            session_id: session_id.clone(),
            requests,
        })
    }
}

impl Session {
    /// Derives the public catalog view from current server policy and host preferences.
    pub(super) fn mcp_snapshot(
        &self,
    ) -> Result<protocol::McpSessionSnapshot, KernelError> {
        let Some(state) = self.mcp.state() else {
            return Ok(protocol::McpSessionSnapshot::default());
        };
        let notifications = state.notifications.lock()?;
        let selection = self.tools.snapshot()?;
        let mut snapshot = state.session.snapshot().as_ref().clone();
        snapshot.revision = notifications.revision;
        for tool in &mut snapshot.catalog.tools {
            tool.disabled =
                selection.selection.disabled.contains(&tool.public_name);
            tool.loaded =
                selection.declares(&tool.public_name, tool.metadata.exposure);
        }
        Ok(snapshot)
    }

    /// Subscribes to both server changes and durable selection changes through one ACP channel.
    pub(super) fn subscribe_mcp_projection(
        &self,
    ) -> Result<Option<broadcast::Receiver<::mcp::McpSessionEvent>>, KernelError>
    {
        self.mcp
            .state()
            .map(|state| {
                state
                    .notifications
                    .lock()
                    .map(|notifications| notifications.sender.subscribe())
                    .map_err(KernelError::from)
            })
            .transpose()
    }

    /// Publishes one monotonic invalidation after the corresponding state has become visible.
    pub(super) fn notify_mcp_change(
        &self,
        change: protocol::McpSessionChange,
        server_id: Option<protocol::McpServerId>,
    ) -> Result<(), KernelError> {
        let Some(state) = self.mcp.state() else {
            return Ok(());
        };
        let mut notifications = state.notifications.lock()?;
        notifications.revision =
            MCP_UI_REVISION.fetch_add(1, Ordering::Relaxed);
        let _ = notifications.sender.send(::mcp::McpSessionEvent {
            revision: notifications.revision,
            server_id,
            change,
        });
        Ok(())
    }

    /// Starts the Session-owned projection from MCP snapshots into the MCP Tool partition.
    pub(super) async fn start_mcp_projection(
        self: &std::sync::Arc<Self>,
    ) -> Result<(), KernelError> {
        let Some(state) = self.mcp.state() else {
            return Ok(());
        };
        let mcp = Arc::clone(&state.session);
        let mut events = mcp.subscribe();
        let mut published = mcp.snapshot();
        // Subscribe before reconciling startup: events between initial discovery and attachment must not be lost.
        self.tools.replace_mcp_partition(
            mcp.tool_registry_from_snapshot(&published)?,
        )?;
        let tools = std::sync::Arc::clone(&self.tools);
        let owner = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            loop {
                let (change, server_id) = match events.recv().await {
                    Ok(event) => (event.change, event.server_id),
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            "MCP Tool projection lagged by {} revisions; loading latest snapshot",
                            skipped
                        );
                        (::mcp::McpSessionChange::Catalog, None)
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let current = mcp.snapshot();
                // Status-only notifications do not invalidate search documents or compiled validators.
                if current.catalog.tools != published.catalog.tools
                    || change == ::mcp::McpSessionChange::Shutdown
                {
                    match mcp.tool_registry_from_snapshot(&current).and_then(
                        |registry| {
                            tools.replace_mcp_partition(registry).map_err(
                                |error| {
                                    ::mcp::McpError::ToolRegistration(
                                        error.to_string(),
                                    )
                                },
                            )
                        },
                    ) {
                        Ok(()) => {}
                        Err(error) => tracing::error!(
                            "MCP Tool projection failed for session '{}': {}",
                            mcp.session_id(),
                            error
                        ),
                    }
                }
                published = current;
                let Some(owner) = owner.upgrade() else {
                    break;
                };
                if let Err(error) = owner.notify_mcp_change(change, server_id) {
                    tracing::error!(
                        "MCP projection notification failed: {}",
                        error
                    );
                }
                if change == ::mcp::McpSessionChange::Shutdown {
                    break;
                }
            }
        });
        *state.projection.lock().await = Some(task);
        Ok(())
    }

    /// Emits one MCP Host event through the active Run, as an MCP Host error.
    async fn emit_mcp_event(
        &self,
        turn_id: TurnId,
        timestamp_ms: TimestampMs,
        payload: AgentEventPayload,
    ) -> Result<(), ::mcp::McpError> {
        self.execution
            .emit_to_run(turn_id, timestamp_ms, payload)
            .await
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))
    }

    /// Publishes and waits for one Turn-scoped ACP elicitation response.
    async fn elicit_mcp(
        &self,
        request: McpElicitationRequest,
    ) -> Result<McpElicitationResult, ::mcp::McpError> {
        let request_id = request.request_id.clone();
        let turn_id = request.context.turn_id.clone();
        let requested_at_ms = request.context.requested_at_ms;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let state = self.mcp.state().ok_or_else(|| {
            ::mcp::McpError::Host(
                "MCP is disabled for this Session".to_string(),
            )
        })?;
        {
            let mut elicitations =
                state.elicitations.lock().map_err(|_poison_error| {
                    ::mcp::McpError::Host(
                        "MCP elicitation lock poisoned".to_string(),
                    )
                })?;
            match elicitations.entry(request_id.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(PendingMcpElicitation {
                        request: request.clone(),
                        sender,
                    });
                }
                Entry::Occupied(_entry) => {
                    return Err(::mcp::McpError::Host(format!(
                        "duplicate MCP elicitation request '{request_id}'"
                    )));
                }
            }
        };
        if let Err(error) = self
            .emit_mcp_event(
                turn_id.clone(),
                requested_at_ms,
                AgentEventPayload::McpElicitationRequested {
                    request: request.clone(),
                },
            )
            .await
        {
            state
                .elicitations
                .lock()
                .map_err(|_poison_error| {
                    ::mcp::McpError::Host(
                        "MCP elicitation lock poisoned".to_string(),
                    )
                })?
                .remove(&request_id);
            return Err(error);
        }
        // The first prompt the Run waits on, MCP or plan approval, blocks it on the user.
        if self.execution.begin_user_wait() {
            tracing::info!(
                "Run requires user action for MCP elicitation {}",
                request_id
            );
            self.emit_mcp_event(
                turn_id.clone(),
                requested_at_ms,
                AgentEventPayload::ActionRequired {
                    request_id: request_id.clone(),
                },
            )
            .await?;
        }
        let cancellation = self
            .execution
            .cancellation()
            .map_err(|error| ::mcp::McpError::Host(error.to_string()))?;
        let result = tokio::select! {
            _ = cancellation.cancelled() => McpElicitationResult {
                action: McpElicitationAction::Cancel,
                content: None,
            },
            response = receiver => response.map_err(|_closed| {
                ::mcp::McpError::Host(format!(
                    "MCP elicitation '{request_id}' response channel closed"
                ))
            })?,
        };
        state
            .elicitations
            .lock()
            .map_err(|_poison_error| {
                ::mcp::McpError::Host(
                    "MCP elicitation lock poisoned".to_string(),
                )
            })?
            .remove(&request_id);
        self.emit_mcp_event(
            turn_id.clone(),
            TimestampMs::now(),
            AgentEventPayload::McpElicitationResolved {
                request_id: request_id.clone(),
                result: result.clone(),
            },
        )
        .await?;
        // The Run is still waiting on this tool, so it resumes foreground work
        // once no other prompt still waits.
        if self.execution.end_user_wait() {
            tracing::info!(
                "Run resumed after MCP elicitation {} was resolved",
                request_id
            );
            self.emit_mcp_event(
                turn_id,
                TimestampMs::now(),
                AgentEventPayload::ActionResolved { request_id },
            )
            .await?;
        }
        Ok(result)
    }

    /// Delivers one ACP response to the exact pending MCP elicitation.
    pub(super) async fn resolve_mcp_elicitation(
        &self,
        request: McpElicitationResponseRequest,
    ) -> Result<(), KernelError> {
        let state = self.mcp.state().ok_or_else(|| {
            KernelError::Protocol(
                "MCP is disabled for this Session".to_string(),
            )
        })?;
        let pending = state
            .elicitations
            .lock()?
            .remove(&request.request_id)
            .ok_or_else(|| {
                // Another client answered first, or the Turn cancelled it.
                tracing::info!(
                    "ignored response for MCP elicitation {} that is no longer pending",
                    request.request_id
                );
                KernelError::ElicitationResolved(request.request_id.clone())
            })?;
        pending.sender.send(request.result).map_err(|_result| {
            tracing::info!(
                "MCP elicitation {} stopped waiting before its response arrived",
                request.request_id
            );
            KernelError::ElicitationResolved(request.request_id.clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};

    use async_trait::async_trait;
    use protocol::{McpHostRequest, McpHostResponse, SessionId};
    use tokio::sync::Mutex as AsyncMutex;
    use tokio_util::sync::CancellationToken;

    use super::{McpLease, SessionMcpHost, SessionMcpRuntime, SessionMcpState};

    /// Process-wide MCP test capture buffer written by the global subscriber.
    static BUFFER: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

    /// Returns the shared capture buffer, installing the global subscriber once.
    ///
    /// `set_default` is thread-local and is lost when a `tokio` task migrates
    /// between worker threads, which makes lifecycle log capture flaky. A global
    /// subscriber retains every emitted log on whichever thread it fires.
    fn buffer() -> &'static Arc<Mutex<Vec<u8>>> {
        BUFFER.get_or_init(|| {
            let buf = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(CapturedLogWriter(Arc::clone(&buf)))
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("global MCP tracing subscriber already installed");
            buf
        })
    }

    /// Shared in-memory writer used to inspect MCP lifecycle logs.
    #[derive(Clone)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl CapturedLogs {
        /// Returns all UTF-8 tracing output written by the subscriber.
        fn content(&self) -> String {
            String::from_utf8(self.0.lock().expect("log lock").clone())
                .expect("UTF-8 logs")
        }
    }

    impl Default for CapturedLogs {
        /// Creates a handle over the shared capture buffer.
        fn default() -> Self {
            CapturedLogs(Arc::clone(buffer()))
        }
    }

    /// One writer handle backed by the shared test buffer.
    struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedLogWriter {
        /// Appends one formatted tracing buffer to the shared capture.
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_poison_error| io::Error::other("log lock poisoned"))?
                .write(buffer)
        }

        /// The in-memory capture has no buffered state to flush.
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
        type Writer = CapturedLogWriter;

        /// Creates a writer handle sharing the captured byte buffer.
        fn make_writer(&'writer self) -> Self::Writer {
            CapturedLogWriter(Arc::clone(&self.0))
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer>
        for CapturedLogWriter
    {
        type Writer = CapturedLogWriter;

        /// Creates a writer handle sharing the captured byte buffer.
        fn make_writer(&'writer self) -> Self::Writer {
            CapturedLogWriter(Arc::clone(&self.0))
        }
    }

    struct RejectingHost;

    #[async_trait]
    impl ::mcp::McpHost for RejectingHost {
        /// Rejects callbacks because the empty MCP Session has no Servers.
        async fn handle(
            &self,
            _request: McpHostRequest,
        ) -> Result<McpHostResponse, ::mcp::McpError> {
            Err(::mcp::McpError::Host(
                "unexpected test Host callback".to_string(),
            ))
        }
    }

    /// Creates one empty MCP Session with an observable shutdown token.
    async fn mcp_session(shutdown: CancellationToken) -> ::mcp::McpSession {
        use ::mcp::McpFactory as _;

        ::mcp::SessionMcpFactory::new(
            Vec::new(),
            Arc::new(::mcp::RmcpConnector::default()),
        )
        .create(
            ::mcp::McpSessionRequest::builder()
                .session_id(
                    SessionId::try_from("session-mcp-cleanup")
                        .expect("session id"),
                )
                .cwd(PathBuf::from("/workspace"))
                .host(Arc::new(RejectingHost) as Arc<dyn ::mcp::McpHost>)
                .shutdown(shutdown)
                .build(),
        )
        .await
        .expect("create empty MCP Session")
    }

    /// Preserves MCP shutdown even when the projection task exits abnormally.
    #[tokio::test]
    async fn shutdown_cancels_mcp_when_projection_join_fails() {
        let shutdown = CancellationToken::new();
        let session = Arc::new(mcp_session(shutdown.clone()).await);
        let failed_projection = tokio::spawn(async {
            panic!("forced projection failure");
        });
        let runtime = SessionMcpRuntime::Enabled(
            SessionMcpState::builder()
                .session(session)
                .host(Arc::new(SessionMcpHost::new(PathBuf::from(
                    "/workspace",
                ))))
                .session_id(
                    SessionId::try_from("session-mcp-cleanup").expect("id"),
                )
                .lease(McpLease::Owned)
                .projection(AsyncMutex::new(Some(failed_projection)))
                .elicitations(Mutex::new(std::collections::HashMap::new()))
                .build(),
        );

        let error = runtime
            .shutdown()
            .await
            .expect_err("projection failure must remain observable");

        assert!(shutdown.is_cancelled());
        assert!(matches!(error, crate::KernelError::Protocol(_)));
    }

    /// MCP shutdown logs both third-party call boundaries with Session context.
    #[tokio::test]
    async fn shutdown_lifecycle_is_logged_with_session_context() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let shutdown = CancellationToken::new();
        let session = Arc::new(mcp_session(shutdown.clone()).await);
        let runtime = SessionMcpRuntime::Enabled(
            SessionMcpState::builder()
                .session(session)
                .host(Arc::new(SessionMcpHost::new(PathBuf::from(
                    "/workspace",
                ))))
                .session_id(
                    SessionId::try_from("session-mcp-cleanup").expect("id"),
                )
                .lease(McpLease::Owned)
                .projection(AsyncMutex::new(None))
                .elicitations(Mutex::new(std::collections::HashMap::new()))
                .build(),
        );

        tracing_futures::WithSubscriber::with_subscriber(
            runtime.shutdown(),
            dispatch,
        )
        .await
        .expect("shut down MCP runtime");

        assert!(shutdown.is_cancelled());
        let output = logs.content();
        for lifecycle in [
            "started MCP shutdown for session session-mcp-cleanup",
            "completed MCP shutdown for session session-mcp-cleanup",
        ] {
            assert!(
                output.contains(lifecycle),
                "missing {lifecycle} in captured logs: {output:?}"
            );
        }
    }
}
