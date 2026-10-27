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
#[derive(typed_builder::TypedBuilder)]
#[builder(builder_method(vis = ""), builder_type(vis = ""))]
pub struct McpSession {
    session_id: SessionId,
    cwd: PathBuf,
    host: Arc<dyn McpHost>,
    shutdown: CancellationToken,
    servers: Arc<[Arc<McpServerRuntime>]>,
    snapshot: Arc<ArcSwap<McpSessionSnapshot>>,
    events: broadcast::Sender<McpSessionEvent>,
    #[builder(default)]
    shutdown_lock: tokio::sync::Mutex<()>,
    #[builder(default)]
    shutdown_complete: AtomicBool,
    revision: Arc<tokio::sync::Mutex<u64>>,
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
            Arc::new(ArcSwap::from_pointee(Self::aggregate(1, &servers, None)));
        let revision = Arc::new(tokio::sync::Mutex::new(1_u64));
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
                        // Publish the revision, snapshot, and notification together;
                        // an atomic counter alone lets an older aggregate win the race.
                        let mut revision = revision.lock().await;
                        *revision = revision.saturating_add(1);
                        let previous = snapshot.load_full();
                        snapshot.store(Arc::new(Self::aggregate(
                            *revision, &servers, Some(&previous),
                        )));
                        let _ = events.send(McpSessionEvent {
                            revision: *revision,
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
        let session = Self::builder()
            .session_id(request.session_id)
            .cwd(request.cwd)
            .host(request.host)
            .shutdown(request.shutdown)
            .servers(servers)
            .snapshot(snapshot)
            .events(events)
            .revision(revision)
            .watchers(tokio::sync::Mutex::new(watchers))
            .build();
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
    pub fn tool_registry(self: &Arc<Self>) -> Result<ToolRegistry, McpError> {
        let snapshot = self.snapshot();
        self.tool_registry_from_snapshot(&snapshot)
    }

    /// Builds definitions from one captured snapshot so consumers can publish that exact revision.
    pub fn tool_registry_from_snapshot(
        self: &Arc<Self>,
        snapshot: &McpSessionSnapshot,
    ) -> Result<ToolRegistry, McpError> {
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
        if self.servers.iter().any(|server| server.allows_resources()) {
            use crate::resource::{McpResourceOperation, McpResourceTool};
            for operation in [
                McpResourceOperation::ListResources,
                McpResourceOperation::ListTemplates,
                McpResourceOperation::Read,
            ] {
                registry
                    .register(Arc::new(McpResourceTool::new(
                        Arc::clone(self),
                        operation,
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
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<McpResourceResult, McpError> {
        self.ensure_resource_access(&request.reference.server_id)?;
        // Aggregate catalog failures are Session policy failures even while the transport stays connected.
        if let Some(status) = self
            .snapshot()
            .servers
            .iter()
            .find(|status| status.server_id == request.reference.server_id)
            && !matches!(
                status.state,
                protocol::McpServerState::Ready
                    | protocol::McpServerState::Degraded
            )
        {
            return Err(McpError::ServerUnavailable {
                server_id: status.server_id.clone(),
                state: status.state,
            });
        }
        self.server(&request.reference.server_id)?
            .read_resource(request, cancellation)
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

    /// Validates resource policy without assuming the URI was present in the catalog.
    pub(crate) fn ensure_resource_access(
        &self,
        server_id: &McpServerId,
    ) -> Result<(), McpError> {
        if self.server(server_id)?.allows_resources() {
            Ok(())
        } else {
            Err(McpError::Service {
                server_id: server_id.clone(),
                message: "resource access is hidden by host policy".into(),
            })
        }
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
        // Shutdown uses the same publication boundary as the Server watchers.
        let mut revision = self.revision.lock().await;
        *revision = revision.saturating_add(1);
        let previous = self.snapshot();
        self.snapshot.store(Arc::new(Self::aggregate(
            *revision,
            &self.servers,
            Some(&previous),
        )));
        let _ = self.events.send(McpSessionEvent {
            revision: *revision,
            server_id: None,
            change: McpSessionChange::Shutdown,
        });
        Ok(())
    }

    /// Rejects ambiguous aggregate names while retaining each affected Server's last valid view.
    fn aggregate(
        revision: u64,
        servers: &[Arc<McpServerRuntime>],
        previous: Option<&McpSessionSnapshot>,
    ) -> McpSessionSnapshot {
        let views: Vec<_> =
            servers.iter().map(|server| server.snapshot()).collect();
        let changed: std::collections::HashSet<_> = views
            .iter()
            .filter(|view| {
                previous.is_none_or(|previous| {
                    !previous
                        .catalog
                        .tools
                        .iter()
                        .filter(|tool| {
                            tool.reference.server_id == view.status.server_id
                        })
                        .eq(view.catalog.tools.iter())
                })
            })
            .map(|view| &view.status.server_id)
            .collect();
        let mut rejected = std::collections::HashSet::new();
        loop {
            let mut owners = std::collections::HashMap::new();
            let mut conflicts = std::collections::HashSet::new();
            for view in &views {
                let server_id = &view.status.server_id;
                let candidate = if rejected.contains(server_id) {
                    previous.map(|previous| &previous.catalog)
                } else {
                    Some(view.catalog.as_ref())
                };
                for tool in candidate
                    .into_iter()
                    .flat_map(|catalog| &catalog.tools)
                    .filter(|tool| &tool.reference.server_id == server_id)
                {
                    if let Some(owner) =
                        owners.insert(tool.public_name.as_str(), server_id)
                    {
                        conflicts.insert(owner);
                        conflicts.insert(server_id);
                    }
                }
            }
            if conflicts.is_empty() {
                break;
            }
            let additional: Vec<_> = conflicts
                .into_iter()
                .filter(|server_id| {
                    changed.contains(server_id) && !rejected.contains(server_id)
                })
                .collect();
            // Previous snapshots are valid; every new conflict must contain a changed contributor.
            assert!(
                !additional.is_empty(),
                "previous MCP catalog must have unique tool names"
            );
            rejected.extend(additional);
        }
        let mut catalog = McpCatalog::default();
        let statuses = views
            .iter()
            .map(|view| {
                let server_id = &view.status.server_id;
                let rejected = rejected.contains(server_id);
                let mut status = view.status.as_ref().clone();
                let partition = if rejected {
                    // Only changed contributors are rejected, so a new catalog cannot evict a healthy peer.
                    let prior_status = previous.and_then(|previous| {
                        previous
                            .servers
                            .iter()
                            .find(|server| &server.server_id == server_id)
                    });
                    status.state = if prior_status.is_some_and(|server| {
                        matches!(
                            server.state,
                            protocol::McpServerState::Ready
                                | protocol::McpServerState::Degraded
                        )
                    }) {
                        protocol::McpServerState::Degraded
                    } else {
                        protocol::McpServerState::Failed
                    };
                    status.revisions = prior_status
                        .map(|server| server.revisions)
                        .unwrap_or_default();
                    status.failure = Some(protocol::McpServerFailure {
                        stage: protocol::McpFailureStage::Discovery,
                        message: "tool public names collide across MCP servers"
                            .into(),
                        occurred_at_ms: protocol::TimestampMs::now(),
                    });
                    previous.map_or_else(McpCatalog::default, |previous| {
                        McpCatalog::builder()
                            .tools(
                                previous
                                    .catalog
                                    .tools
                                    .iter()
                                    .filter(|item| {
                                        &item.reference.server_id == server_id
                                    })
                                    .cloned()
                                    .collect(),
                            )
                            .prompts(
                                previous
                                    .catalog
                                    .prompts
                                    .iter()
                                    .filter(|item| {
                                        &item.reference.server_id == server_id
                                    })
                                    .cloned()
                                    .collect(),
                            )
                            .resources(
                                previous
                                    .catalog
                                    .resources
                                    .iter()
                                    .filter(|item| {
                                        &item.reference.server_id == server_id
                                    })
                                    .cloned()
                                    .collect(),
                            )
                            .resource_templates(
                                previous
                                    .catalog
                                    .resource_templates
                                    .iter()
                                    .filter(|item| &item.server_id == server_id)
                                    .cloned()
                                    .collect(),
                            )
                            .build()
                    })
                } else {
                    view.catalog.as_ref().clone()
                };
                status.counts = partition.counts();
                catalog.tools.extend(partition.tools);
                catalog.prompts.extend(partition.prompts);
                catalog.resources.extend(partition.resources);
                catalog
                    .resource_templates
                    .extend(partition.resource_templates);
                status
            })
            .collect();
        McpSessionSnapshot {
            revision,
            servers: statuses,
            catalog,
        }
    }
}
