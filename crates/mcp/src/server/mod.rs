//! Session-owned lifecycle, client, and immutable catalog for one configured Server.

mod lifecycle;

use std::sync::Arc;

use arc_swap::ArcSwap;
use protocol::{
    McpAuthorizationRequest, McpAuthorizationResult, McpCatalog,
    McpCatalogRevisions, McpCompletionRequest, McpCompletionResult,
    McpFailureStage, McpOAuthState, McpOAuthStatus, McpPromptRequest,
    McpPromptResult, McpRequestContext, McpResourceRequest, McpResourceResult,
    McpServerState, McpServerStatus, McpToolRequest, McpToolResult,
    TimestampMs,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub use lifecycle::McpServerStateMachine;

use crate::{
    McpAuthentication, McpClient, McpClientEvent, McpConnector, McpError,
    McpHost, McpProgressSink, McpRequestControl, McpServerChange, McpTransport,
    RuntimeMcpServer,
};

/// Runtime resources and atomic read views owned by one Session Server.
pub struct McpServerRuntime {
    config: RuntimeMcpServer,
    connector: Arc<dyn McpConnector>,
    session_id: protocol::SessionId,
    host: Arc<dyn McpHost>,
    client: tokio::sync::RwLock<Option<Arc<dyn McpClient>>>,
    catalog: ArcSwap<McpCatalog>,
    status: ArcSwap<McpServerStatus>,
    state: tokio::sync::Mutex<McpServerStateMachine>,
    session_shutdown: CancellationToken,
    lifecycle: tokio::sync::RwLock<CancellationToken>,
    changes: broadcast::Sender<McpServerChange>,
    watcher: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    lifecycle_operation: tokio::sync::Mutex<()>,
}

impl McpServerRuntime {
    /// Bootstraps one Server while converting every Server-local failure into status.
    pub async fn bootstrap(
        config: RuntimeMcpServer,
        connector: Arc<dyn McpConnector>,
        session_id: protocol::SessionId,
        host: Arc<dyn McpHost>,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        let initial_state = if config.enabled {
            McpServerState::Starting
        } else {
            McpServerState::Disabled
        };
        let oauth = match &config.transport {
            McpTransport::StreamableHttp(transport)
                if matches!(
                    transport.authentication,
                    McpAuthentication::OAuth(_)
                ) =>
            {
                McpOAuthStatus::builder()
                    .state(McpOAuthState::Authorizing)
                    .build()
            }
            _ => McpOAuthStatus::default(),
        };
        let initial_status = McpServerStatus::builder()
            .server_id(config.server_id.clone())
            .protocol(config.protocol)
            .transport(config.transport_kind())
            .state(initial_state)
            .revisions(McpCatalogRevisions::default())
            .counts(Default::default())
            .oauth(oauth)
            .updated_at_ms(TimestampMs::now())
            .build();
        let (changes, _receiver) = broadcast::channel(64);
        let runtime = Arc::new(Self {
            config,
            connector,
            session_id,
            host,
            client: tokio::sync::RwLock::new(None),
            catalog: ArcSwap::from_pointee(McpCatalog::default()),
            status: ArcSwap::from_pointee(initial_status.clone()),
            state: tokio::sync::Mutex::new(McpServerStateMachine::new(
                initial_status,
            )),
            lifecycle: tokio::sync::RwLock::new(shutdown.child_token()),
            session_shutdown: shutdown,
            changes,
            watcher: tokio::sync::Mutex::new(None),
            lifecycle_operation: tokio::sync::Mutex::new(()),
        });
        if !runtime.config.enabled {
            return runtime;
        }
        runtime.connect().await;
        runtime
    }

    /// Runs one exact negotiation and discovery lifecycle from Starting state.
    async fn connect(self: &Arc<Self>) {
        let current = self.status().state;
        if matches!(current, McpServerState::Failed | McpServerState::Stopped) {
            let mut state = self.state.lock().await;
            if state
                .transition(McpServerState::Starting, TimestampMs::now())
                .is_err()
            {
                return;
            }
            self.status.store(Arc::new(state.status().clone()));
            let _ = self.changes.send(McpServerChange::Status);
        }
        {
            let mut state = self.state.lock().await;
            if state
                .transition(McpServerState::Negotiating, TimestampMs::now())
                .is_err()
            {
                return;
            }
            self.status.store(Arc::new(state.status().clone()));
        }
        let client = match self
            .connector
            .connect(&self.config, &self.session_id, Arc::clone(&self.host))
            .await
        {
            Ok(client) => client,
            Err(McpError::AuthorizationRequired(request)) => {
                self.await_authorization(request).await;
                return;
            }
            Err(error) => {
                if matches!(
                    &self.config.transport,
                    McpTransport::StreamableHttp(transport)
                        if matches!(
                            transport.authentication,
                            McpAuthentication::OAuth(_)
                        )
                ) {
                    let mut state = self.state.lock().await;
                    state.set_oauth(
                        McpOAuthStatus::builder()
                            .state(McpOAuthState::Failed)
                            .build(),
                    );
                    self.status.store(Arc::new(state.status().clone()));
                }
                let stage = match error {
                    McpError::ProtocolMismatch { .. } => {
                        McpFailureStage::Negotiation
                    }
                    McpError::OAuthNotInitialized
                    | McpError::BearerToken(_) => {
                        McpFailureStage::Authentication
                    }
                    _ => McpFailureStage::Transport,
                };
                self.fail(stage, error.to_string()).await;
                return;
            }
        };
        self.activate_client(client).await;
    }

    /// Publishes a safe browser authorization challenge without failing bootstrap.
    async fn await_authorization(&self, request: McpAuthorizationRequest) {
        let mut state = self.state.lock().await;
        state.set_oauth(
            McpOAuthStatus::builder()
                .state(McpOAuthState::Unauthenticated)
                .scopes(request.scopes)
                .authorization_url(Some(request.authorization_url))
                .build(),
        );
        self.status.store(Arc::new(state.status().clone()));
        let _ = self.changes.send(McpServerChange::Status);
    }

    /// Discovers and publishes one successfully negotiated client.
    async fn activate_client(self: &Arc<Self>, client: Arc<dyn McpClient>) {
        {
            let mut state = self.state.lock().await;
            if let McpTransport::StreamableHttp(transport) =
                &self.config.transport
                && let McpAuthentication::OAuth(settings) =
                    &transport.authentication
            {
                state.set_oauth(
                    McpOAuthStatus::builder()
                        .state(McpOAuthState::Ready)
                        .scopes(settings.scopes.clone())
                        .build(),
                );
            }
            if state
                .transition(McpServerState::Discovering, TimestampMs::now())
                .is_err()
            {
                let _ = client.shutdown().await;
                return;
            }
            self.status.store(Arc::new(state.status().clone()));
        }
        let catalog = match self.discover(&client).await {
            Ok(catalog) => catalog,
            Err(error) => {
                let _ = client.shutdown().await;
                self.fail(McpFailureStage::Discovery, error.to_string())
                    .await;
                return;
            }
        };
        {
            let mut state = self.state.lock().await;
            if state
                .ready(
                    client.server_info().cloned(),
                    &catalog,
                    TimestampMs::now(),
                )
                .is_err()
            {
                let _ = client.shutdown().await;
                return;
            }
            self.status.store(Arc::new(state.status().clone()));
        }
        self.catalog.store(Arc::new(catalog));
        let receiver = client.subscribe_changes();
        *self.client.write().await = Some(client);
        let _ = self.changes.send(McpServerChange::Catalog);
        if let Some(receiver) = receiver {
            self.start_change_watcher(receiver).await;
        }
    }

    /// Returns the latest immutable status snapshot.
    #[must_use]
    pub fn status(&self) -> Arc<McpServerStatus> {
        self.status.load_full()
    }

    /// Returns the latest immutable complete Server catalog.
    #[must_use]
    pub fn catalog(&self) -> Arc<McpCatalog> {
        self.catalog.load_full()
    }

    /// Subscribes the owning Session to status and catalog publications.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<McpServerChange> {
        self.changes.subscribe()
    }

    /// Returns a client only while this Server is ready for new operations.
    async fn ready_client(&self) -> Result<Arc<dyn McpClient>, McpError> {
        let status = self.status();
        if status.state != McpServerState::Ready {
            return Err(McpError::ServerUnavailable {
                server_id: self.config.server_id.clone(),
                state: status.state,
            });
        }
        self.client
            .read()
            .await
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| McpError::ServerUnavailable {
                server_id: self.config.server_id.clone(),
                state: self.status().state,
            })
    }

    /// Invokes one Tool only while this Server remains ready for new work.
    pub async fn call_tool(
        &self,
        request: McpToolRequest,
        context: McpRequestContext,
        cancellation: &CancellationToken,
        progress: Arc<dyn McpProgressSink>,
    ) -> Result<McpToolResult, McpError> {
        let client = self.ready_client().await?;
        let lifecycle = self.lifecycle.read().await.clone();
        client
            .call_tool(
                request,
                McpRequestControl::builder()
                    .context(context)
                    .cancellation(cancellation.clone())
                    .lifecycle(lifecycle)
                    .idle_timeout(self.config.request_timeout)
                    .total_timeout(self.config.mrtr.total_timeout)
                    .max_rounds(self.config.mrtr.max_rounds)
                    .progress(progress)
                    .build(),
            )
            .await
    }

    /// Retrieves one Prompt while enforcing lifecycle cancellation and request timeout.
    pub async fn get_prompt(
        &self,
        request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        let client = self.ready_client().await?;
        let lifecycle = self.lifecycle.read().await.clone();
        tokio::select! {
            _ = lifecycle.cancelled() => {
                Err(McpError::RequestCancelled(self.config.server_id.clone()))
            }
            result = tokio::time::timeout(
                self.config.request_timeout,
                client.get_prompt(request),
            ) => result.map_err(|_elapsed| {
                McpError::RequestTimeout(self.config.server_id.clone())
            })?,
        }
    }

    /// Reads one Resource while enforcing lifecycle cancellation and request timeout.
    pub async fn read_resource(
        &self,
        request: McpResourceRequest,
    ) -> Result<McpResourceResult, McpError> {
        let client = self.ready_client().await?;
        let lifecycle = self.lifecycle.read().await.clone();
        tokio::select! {
            _ = lifecycle.cancelled() => {
                Err(McpError::RequestCancelled(self.config.server_id.clone()))
            }
            result = tokio::time::timeout(
                self.config.request_timeout,
                client.read_resource(request),
            ) => result.map_err(|_elapsed| {
                McpError::RequestTimeout(self.config.server_id.clone())
            })?,
        }
    }

    /// Completes one argument while enforcing lifecycle cancellation and request timeout.
    pub async fn complete(
        &self,
        request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        let client = self.ready_client().await?;
        let lifecycle = self.lifecycle.read().await.clone();
        tokio::select! {
            _ = lifecycle.cancelled() => {
                Err(McpError::RequestCancelled(self.config.server_id.clone()))
            }
            result = tokio::time::timeout(
                self.config.request_timeout,
                client.complete(request),
            ) => result.map_err(|_elapsed| {
                McpError::RequestTimeout(self.config.server_id.clone())
            })?,
        }
    }

    /// Stops the client and clears the published catalog.
    pub async fn shutdown(&self) {
        let _operation = self.lifecycle_operation.lock().await;
        self.stop().await;
    }

    /// Cancels current work, stops the transport, then reruns the configured exact lifecycle.
    pub async fn reconnect(self: &Arc<Self>) -> Result<(), McpError> {
        let _operation = self.lifecycle_operation.lock().await;
        if !self.config.enabled {
            return Err(McpError::ServerUnavailable {
                server_id: self.config.server_id.clone(),
                state: McpServerState::Disabled,
            });
        }
        self.stop().await;
        *self.lifecycle.write().await = self.session_shutdown.child_token();
        self.connect().await;
        if self.status().state == McpServerState::Ready {
            Ok(())
        } else {
            Err(McpError::ServerUnavailable {
                server_id: self.config.server_id.clone(),
                state: self.status().state,
            })
        }
    }

    /// Completes one retained OAuth round and resumes discovery on this Server runtime.
    pub async fn continue_authorization(
        self: &Arc<Self>,
        result: McpAuthorizationResult,
    ) -> Result<(), McpError> {
        let _operation = self.lifecycle_operation.lock().await;
        if self.status().oauth.state != McpOAuthState::Unauthenticated {
            return Err(McpError::AuthorizationNotPending {
                session_id: self.session_id.clone(),
                server_id: self.config.server_id.clone(),
            });
        }
        {
            let mut state = self.state.lock().await;
            let scopes = state.status().oauth.scopes.clone();
            state.set_oauth(
                McpOAuthStatus::builder()
                    .state(McpOAuthState::Authorizing)
                    .scopes(scopes)
                    .build(),
            );
            self.status.store(Arc::new(state.status().clone()));
            let _ = self.changes.send(McpServerChange::Status);
        }
        let client = match self
            .connector
            .continue_authorization(
                &self.config,
                &self.session_id,
                result,
                Arc::clone(&self.host),
            )
            .await
        {
            Ok(client) => client,
            Err(error) => {
                let mut state = self.state.lock().await;
                state.set_oauth(
                    McpOAuthStatus::builder()
                        .state(McpOAuthState::Failed)
                        .build(),
                );
                self.status.store(Arc::new(state.status().clone()));
                drop(state);
                self.fail(McpFailureStage::Authentication, error.to_string())
                    .await;
                return Err(error);
            }
        };
        self.activate_client(client).await;
        if self.status().state == McpServerState::Ready {
            Ok(())
        } else {
            Err(McpError::ServerUnavailable {
                server_id: self.config.server_id.clone(),
                state: self.status().state,
            })
        }
    }

    /// Stops one active lifecycle and publishes an empty catalog before reconnect or teardown.
    async fn stop(&self) {
        if matches!(
            self.status.load().state,
            McpServerState::Disabled | McpServerState::Stopped
        ) {
            return;
        }
        {
            let mut state = self.state.lock().await;
            if state
                .transition(McpServerState::Stopping, TimestampMs::now())
                .is_ok()
            {
                self.status.store(Arc::new(state.status().clone()));
            }
        }
        self.catalog.store(Arc::new(McpCatalog::default()));
        let _ = self.changes.send(McpServerChange::Catalog);
        self.lifecycle.read().await.cancel();
        self.connector
            .cancel_authorization(&self.session_id, &self.config.server_id)
            .await;
        if let Some(watcher) = self.watcher.lock().await.take()
            && let Err(error) = watcher.await
            && !error.is_cancelled()
        {
            tracing::warn!(
                "MCP server '{}' synchronization task failed: {}",
                self.config.server_id,
                error
            );
        }
        let client = self.client.write().await.take();
        if let Some(client) = client
            && let Err(error) = client.shutdown().await
        {
            self.fail(McpFailureStage::Shutdown, error.to_string())
                .await;
            return;
        }
        let mut state = self.state.lock().await;
        if state
            .transition(McpServerState::Stopped, TimestampMs::now())
            .is_ok()
        {
            self.status.store(Arc::new(state.status().clone()));
            let _ = self.changes.send(McpServerChange::Status);
        }
    }

    /// Records one safe terminal failure and atomically publishes its status.
    async fn fail(&self, stage: McpFailureStage, message: String) {
        let mut state = self.state.lock().await;
        if state.fail(stage, message, TimestampMs::now()).is_ok() {
            self.status.store(Arc::new(state.status().clone()));
            let _ = self.changes.send(McpServerChange::Status);
        }
    }

    /// Starts one normalized notification loop for complete-catalog refresh transactions.
    async fn start_change_watcher(
        self: &Arc<Self>,
        mut receiver: broadcast::Receiver<McpClientEvent>,
    ) {
        let runtime = Arc::clone(self);
        let lifecycle = self.lifecycle.read().await.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = lifecycle.cancelled() => break,
                    event = receiver.recv() => match event {
                        Ok(_event) => runtime.refresh_catalog().await,
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                "MCP server '{}' synchronization lagged by {} notifications",
                                runtime.config.server_id,
                                skipped
                            );
                            runtime.refresh_catalog().await;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        });
        *self.watcher.lock().await = Some(task);
    }

    /// Revalidates the complete paginated catalog and retains last-good data on failure.
    async fn refresh_catalog(&self) {
        let client = self.client.read().await.as_ref().map(Arc::clone);
        let Some(client) = client else {
            return;
        };
        match self.discover(&client).await {
            Ok(catalog) => {
                let mut state = self.state.lock().await;
                if let Err(error) =
                    state.catalog_refreshed(&catalog, TimestampMs::now())
                {
                    tracing::warn!(
                        "MCP server '{}' rejected catalog refresh state: {}",
                        self.config.server_id,
                        error
                    );
                    return;
                }
                self.catalog.store(Arc::new(catalog));
                self.status.store(Arc::new(state.status().clone()));
                let _ = self.changes.send(McpServerChange::Catalog);
            }
            Err(error) => {
                let mut state = self.state.lock().await;
                if state.degrade(error.to_string(), TimestampMs::now()).is_ok()
                {
                    self.status.store(Arc::new(state.status().clone()));
                    let _ = self.changes.send(McpServerChange::Status);
                }
            }
        }
    }

    /// Discovers one complete catalog under the Server lifecycle and request deadline.
    async fn discover(
        &self,
        client: &Arc<dyn McpClient>,
    ) -> Result<McpCatalog, McpError> {
        let lifecycle = self.lifecycle.read().await.clone();
        tokio::select! {
            _ = lifecycle.cancelled() => {
                Err(McpError::RequestCancelled(self.config.server_id.clone()))
            }
            result = tokio::time::timeout(
                self.config.request_timeout,
                client.discover(),
            ) => result.map_err(|_elapsed| {
                McpError::RequestTimeout(self.config.server_id.clone())
            })?,
        }
    }
}
