//! Session-owned MCP Server runtimes and atomic aggregate Snapshot publication.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use protocol::{
    McpAuthorizationContinueRequest, McpCatalog, McpCompletionRequest,
    McpCompletionResult, McpPromptRequest, McpPromptResult, McpResourceRequest,
    McpResourceResult, McpServerId, McpSessionSnapshot, SessionId,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tools::ToolRegistry;

use crate::{
    McpError, McpHost, McpServerChange, McpServerRuntime, McpSessionChange,
    McpSessionEvent,
};

/// Dependencies and identity used to create one Session-owned MCP runtime.
#[derive(typed_builder::TypedBuilder)]
pub struct McpSessionRequest {
    /// Agent Session that owns every resulting Server connection.
    pub session_id: SessionId,
    /// Session working directory used by Roots and stdio Servers.
    pub cwd: PathBuf,
    /// Kernel-owned Host callback implementation.
    pub host: Arc<dyn McpHost>,
    /// Parent cancellation token for explicit Session teardown.
    pub shutdown: CancellationToken,
}

/// Session-owned Server collection with one atomically published read view.
pub struct McpSession {
    session_id: SessionId,
    cwd: PathBuf,
    host: Arc<dyn McpHost>,
    shutdown: CancellationToken,
    servers: Arc<[Arc<McpServerRuntime>]>,
    snapshot: Arc<ArcSwap<McpSessionSnapshot>>,
    events: broadcast::Sender<McpSessionEvent>,
    shutdown_lock: tokio::sync::Mutex<()>,
    shutdown_complete: AtomicBool,
    revision: Arc<std::sync::atomic::AtomicU64>,
    watchers: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl McpSession {
    /// Creates a Session after all concurrent Server bootstrap operations settle.
    pub(crate) fn new(
        request: McpSessionRequest,
        servers: Vec<Arc<McpServerRuntime>>,
    ) -> Self {
        let (events, _receiver) = broadcast::channel(128);
        let servers = Arc::<[Arc<McpServerRuntime>]>::from(servers);
        let snapshot =
            Arc::new(ArcSwap::from_pointee(Self::aggregate(1, &servers)));
        let revision = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let watchers = servers
            .iter()
            .map(|server| {
                let server_id = server.status().server_id.clone();
                let mut changes = server.subscribe();
                let servers = Arc::clone(&servers);
                let snapshot = Arc::clone(&snapshot);
                let events = events.clone();
                let revision = Arc::clone(&revision);
                let shutdown = request.shutdown.clone();
                tokio::spawn(async move {
                    loop {
                        let change = tokio::select! {
                            _ = shutdown.cancelled() => break,
                            change = changes.recv() => match change {
                                Ok(change) => change,
                                Err(broadcast::error::RecvError::Lagged(_)) => {
                                    McpServerChange::Catalog
                                }
                                Err(broadcast::error::RecvError::Closed) => break,
                            }
                        };
                        let next = revision
                            .fetch_add(1, Ordering::AcqRel)
                            .saturating_add(1);
                        snapshot.store(Arc::new(Self::aggregate(
                            next, &servers,
                        )));
                        let _ = events.send(McpSessionEvent {
                            revision: next,
                            server_id: Some(server_id.clone()),
                            change: match change {
                                McpServerChange::Status => {
                                    McpSessionChange::ServerStatus
                                }
                                McpServerChange::Catalog => {
                                    McpSessionChange::Catalog
                                }
                            },
                        });
                    }
                })
            })
            .collect();
        let session = Self {
            session_id: request.session_id,
            cwd: request.cwd,
            host: request.host,
            shutdown: request.shutdown,
            servers,
            snapshot,
            events,
            shutdown_lock: tokio::sync::Mutex::new(()),
            shutdown_complete: AtomicBool::new(false),
            revision,
            watchers: tokio::sync::Mutex::new(watchers),
        };
        let _ = session.events.send(McpSessionEvent {
            revision: 1,
            server_id: None,
            change: McpSessionChange::Bootstrap,
        });
        session
    }

    /// Returns the Agent Session identifier owning these MCP connections.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the Session working directory used for MCP Host Roots.
    #[must_use]
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// Returns the Kernel-owned Host callback boundary.
    #[must_use]
    pub fn host(&self) -> &Arc<dyn McpHost> {
        &self.host
    }

    /// Loads one internally consistent immutable status and Catalog view.
    #[must_use]
    pub fn snapshot(&self) -> Arc<McpSessionSnapshot> {
        self.snapshot.load_full()
    }

    /// Subscribes to lightweight revision notifications for future Snapshot reads.
    pub fn subscribe(&self) -> broadcast::Receiver<McpSessionEvent> {
        self.events.subscribe()
    }

    /// Builds one immutable agent Tool registry from the currently published catalog.
    pub fn tool_registry(&self) -> Result<ToolRegistry, McpError> {
        let snapshot = self.snapshot();
        let mut registry = ToolRegistry::default();
        for server in self.servers.iter() {
            let server_id = &server.status().server_id;
            for info in snapshot
                .catalog
                .tools
                .iter()
                .filter(|info| &info.reference.server_id == server_id)
            {
                registry
                    .register(Arc::new(crate::McpAgentTool::new(
                        info.clone(),
                        Arc::clone(server),
                        snapshot.revision,
                    )))
                    .map_err(|error| {
                        McpError::ToolRegistration(error.to_string())
                    })?;
            }
        }
        Ok(registry)
    }

    /// Explicitly reconnects one configured Server without protocol fallback.
    pub async fn reconnect(
        &self,
        server_id: &protocol::McpServerId,
    ) -> Result<(), McpError> {
        self.server(server_id)?.reconnect().await
    }

    /// Completes one retained OAuth round through its exact Session/Server route.
    pub async fn continue_authorization(
        &self,
        request: McpAuthorizationContinueRequest,
    ) -> Result<(), McpError> {
        if request.session_id != self.session_id {
            return Err(McpError::AuthorizationNotPending {
                session_id: request.session_id,
                server_id: request.server_id,
            });
        }
        self.server(&request.server_id)?
            .continue_authorization(request.result)
            .await
    }

    /// Retrieves one Prompt through its structured Server reference.
    pub async fn get_prompt(
        &self,
        request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        self.server(&request.reference.server_id)?
            .get_prompt(request)
            .await
    }

    /// Reads one Resource through its structured Server reference.
    pub async fn read_resource(
        &self,
        request: McpResourceRequest,
    ) -> Result<McpResourceResult, McpError> {
        self.server(&request.reference.server_id)?
            .read_resource(request)
            .await
    }

    /// Completes one Prompt or Resource Template argument through its owning Server.
    pub async fn complete(
        &self,
        request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        self.server(request.target.server_id())?
            .complete(request)
            .await
    }

    /// Resolves one configured Server without parsing a display or Tool name.
    fn server(
        &self,
        server_id: &McpServerId,
    ) -> Result<Arc<McpServerRuntime>, McpError> {
        self.servers
            .iter()
            .find(|server| &server.status().server_id == server_id)
            .map(Arc::clone)
            .ok_or_else(|| McpError::ServerNotFound(server_id.clone()))
    }

    /// Stops all Servers concurrently and publishes one higher empty-Catalog revision.
    pub async fn shutdown(&self) -> Result<(), McpError> {
        let _guard = self.shutdown_lock.lock().await;
        if self.shutdown_complete.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.shutdown.cancel();
        futures::future::join_all(
            self.servers.iter().map(|server| server.shutdown()),
        )
        .await;
        for watcher in self.watchers.lock().await.drain(..) {
            if let Err(error) = watcher.await
                && !error.is_cancelled()
            {
                tracing::warn!("MCP Session watcher failed: {}", error);
            }
        }
        let revision = self
            .revision
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        self.snapshot
            .store(Arc::new(Self::aggregate(revision, &self.servers)));
        let _ = self.events.send(McpSessionEvent {
            revision,
            server_id: None,
            change: McpSessionChange::Shutdown,
        });
        Ok(())
    }

    /// Aggregates configuration-ordered Server views into one atomic Session Snapshot.
    fn aggregate(
        revision: u64,
        servers: &[Arc<McpServerRuntime>],
    ) -> McpSessionSnapshot {
        let mut catalog = McpCatalog::default();
        let statuses = servers
            .iter()
            .map(|server| {
                let server_catalog = server.catalog();
                catalog.tools.extend(server_catalog.tools.clone());
                catalog.prompts.extend(server_catalog.prompts.clone());
                catalog.resources.extend(server_catalog.resources.clone());
                catalog
                    .resource_templates
                    .extend(server_catalog.resource_templates.clone());
                server.status().as_ref().clone()
            })
            .collect();
        McpSessionSnapshot {
            revision,
            servers: statuses,
            catalog,
        }
    }
}
