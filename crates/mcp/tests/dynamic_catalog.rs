use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    McpClient, McpClientEvent, McpConnector, McpError, McpFactory, McpHost,
    McpMrtrPolicy, McpServerRuntime, McpSessionRequest, McpStdioTransport,
    McpTransport, RuntimeMcpServer, SessionMcpFactory,
};
use protocol::{
    McpCatalog, McpCompletionRequest, McpCompletionResult, McpHostRequest,
    McpHostResponse, McpPromptRequest, McpPromptResult, McpProtocolVersion,
    McpResourceRequest, McpResourceResult, McpServerCapabilities, McpServerId,
    McpServerImplementation, McpServerState, McpToolInfo, McpToolRef,
    McpToolRequest, McpToolResult, SessionId,
};
use tokio::sync::Notify;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Connector returning one shared client so tests can mutate its remote catalog.
struct DynamicConnector {
    client: Arc<DynamicClient>,
}

/// Mutable fake Server with an explicit standards-notification stream.
struct DynamicClient {
    server_id: McpServerId,
    capabilities: McpServerCapabilities,
    catalog: RwLock<McpCatalog>,
    fail_discovery: AtomicBool,
    hang_discovery: AtomicBool,
    discovery_started: Notify,
    discovery_release: Notify,
    events: broadcast::Sender<McpClientEvent>,
}

/// Host rejecting callbacks because catalog synchronization does not invoke them.
struct RejectingHost;

#[async_trait]
impl McpHost for RejectingHost {
    /// Rejects any unexpected Host interaction.
    async fn handle(
        &self,
        _request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        Err(McpError::Host("unexpected Host request".to_string()))
    }
}

impl DynamicClient {
    /// Creates one Tool-only fake Server with an initial catalog revision.
    fn new(server_id: McpServerId) -> Self {
        let (events, _receiver) = broadcast::channel(16);
        Self {
            catalog: RwLock::new(
                McpCatalog::builder()
                    .tools(vec![tool(&server_id, "old")])
                    .build(),
            ),
            server_id,
            capabilities: McpServerCapabilities::builder()
                .tools(true)
                .tools_list_changed(true)
                .build(),
            fail_discovery: AtomicBool::new(false),
            hang_discovery: AtomicBool::new(false),
            discovery_started: Notify::new(),
            discovery_release: Notify::new(),
            events,
        }
    }

    /// Replaces the simulated complete remote catalog before notifying the Host.
    fn replace_tool(&self, remote_name: &str) {
        *self.catalog.write().expect("catalog lock") = McpCatalog::builder()
            .tools(vec![tool(&self.server_id, remote_name)])
            .build();
    }

    /// Emits one normalized Tool-list invalidation.
    fn notify_tools_changed(&self) {
        let _ = self.events.send(McpClientEvent::ToolsChanged);
    }

    /// Makes the next catalog refresh remain pending until lifecycle cancellation.
    fn hang_catalog_refresh(&self) {
        self.hang_discovery.store(true, Ordering::SeqCst);
    }

    /// Releases a deliberately paused discovery request for lifecycle ordering tests.
    fn release_catalog_refresh(&self) {
        self.hang_discovery.store(false, Ordering::SeqCst);
        self.discovery_release.notify_one();
    }
}

#[async_trait]
impl McpConnector for DynamicConnector {
    /// Returns the shared dynamic client without opening a transport.
    async fn connect(
        &self,
        _server: &mcp::RuntimeMcpServer,
        _session_id: &SessionId,
        _host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        Ok(Arc::clone(&self.client) as Arc<dyn McpClient>)
    }
}

#[async_trait]
impl McpClient for DynamicClient {
    /// Returns the fake Server route owner.
    fn server_id(&self) -> &McpServerId {
        &self.server_id
    }

    /// Uses the exact Legacy revision for this normalized synchronization test.
    fn protocol(&self) -> McpProtocolVersion {
        McpProtocolVersion::V2025_11_25
    }

    /// Omits optional implementation metadata.
    fn server_info(&self) -> Option<&McpServerImplementation> {
        None
    }

    /// Declares Tool list-change support.
    fn capabilities(&self) -> &McpServerCapabilities {
        &self.capabilities
    }

    /// Returns the latest complete catalog or a deterministic refresh failure.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        if self.hang_discovery.load(Ordering::SeqCst) {
            self.discovery_started.notify_one();
            self.discovery_release.notified().await;
        }
        if self.fail_discovery.load(Ordering::SeqCst) {
            return Err(McpError::Service {
                server_id: self.server_id.clone(),
                message: "refresh unavailable".to_string(),
            });
        }
        Ok(self.catalog.read().expect("catalog lock").clone())
    }

    /// Subscribes one runtime watcher to normalized Server changes.
    fn subscribe_changes(&self) -> Option<broadcast::Receiver<McpClientEvent>> {
        Some(self.events.subscribe())
    }

    /// Tool invocation is outside catalog synchronization coverage.
    async fn call_tool(
        &self,
        _request: McpToolRequest,
        _control: mcp::McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        unreachable!("Tool invocation is not exercised")
    }

    /// Prompt retrieval is outside catalog synchronization coverage.
    async fn get_prompt(
        &self,
        _request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        unreachable!("Prompt retrieval is not exercised")
    }

    /// Resource reading is outside catalog synchronization coverage.
    async fn read_resource(
        &self,
        _request: McpResourceRequest,
    ) -> Result<McpResourceResult, McpError> {
        unreachable!("Resource reading is not exercised")
    }

    /// Completion is outside catalog synchronization coverage.
    async fn complete(
        &self,
        _request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        unreachable!("Completion is not exercised")
    }

    /// Fake shutdown has no transport resources.
    async fn shutdown(&self) -> Result<(), McpError> {
        Ok(())
    }
}

/// Builds one namespaced Tool descriptor for the mutable fake Server.
fn tool(server_id: &McpServerId, remote_name: &str) -> McpToolInfo {
    let reference = McpToolRef {
        server_id: server_id.clone(),
        remote_name: remote_name.to_string(),
    };
    McpToolInfo::builder()
        .reference(reference.clone())
        .public_name(reference.public_name())
        .input_schema(serde_json::json!({ "type": "object" }))
        .build()
}

/// Builds one enabled Legacy Server config without opening a real transport.
fn runtime_server() -> RuntimeMcpServer {
    RuntimeMcpServer::builder()
        .server_id(McpServerId::try_from("dynamic").expect("Server id"))
        .enabled(true)
        .protocol(McpProtocolVersion::V2025_11_25)
        .startup_timeout(Duration::from_secs(1))
        .request_timeout(Duration::from_secs(1))
        .mrtr(McpMrtrPolicy {
            max_rounds: NonZeroU32::new(2).expect("non-zero rounds"),
            total_timeout: Duration::from_secs(2),
        })
        .transport(McpTransport::Stdio(Box::new(McpStdioTransport {
            command: "unused".to_string(),
            args: Vec::new(),
            env: Default::default(),
        })))
        .build()
}

/// Builds one independent Session request for the dynamic test.
fn session_request() -> McpSessionRequest {
    McpSessionRequest::builder()
        .session_id(SessionId::try_from("dynamic-session").expect("Session id"))
        .cwd(PathBuf::from("/workspace"))
        .host(Arc::new(RejectingHost))
        .shutdown(CancellationToken::new())
        .build()
}

#[tokio::test]
async fn publishes_atomic_catalog_revisions_and_keeps_last_good_on_failure() {
    let server_id = McpServerId::try_from("dynamic").expect("Server id");
    let client = Arc::new(DynamicClient::new(server_id));
    let factory = SessionMcpFactory::new(
        vec![runtime_server()],
        Arc::new(DynamicConnector {
            client: Arc::clone(&client),
        }),
    );
    let session = factory.create(session_request()).await.expect("Session");
    let mut events = session.subscribe();

    client.replace_tool("new");
    client.notify_tools_changed();
    tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("catalog event timeout")
        .expect("catalog event");
    let refreshed = session.snapshot();
    assert!(refreshed.revision > 1);
    assert_eq!(refreshed.catalog.tools[0].reference.remote_name, "new");

    client.fail_discovery.store(true, Ordering::SeqCst);
    client.notify_tools_changed();
    tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("degraded event timeout")
        .expect("degraded event");
    let degraded = session.snapshot();
    assert_eq!(degraded.servers[0].state, McpServerState::Degraded);
    assert_eq!(degraded.catalog.tools[0].reference.remote_name, "new");
}

#[tokio::test]
async fn explicit_reconnect_withdraws_then_republishes_the_exact_server() {
    let server_id = McpServerId::try_from("dynamic").expect("Server id");
    let client = Arc::new(DynamicClient::new(server_id.clone()));
    let factory = SessionMcpFactory::new(
        vec![runtime_server()],
        Arc::new(DynamicConnector {
            client: Arc::clone(&client),
        }),
    );
    let session = factory.create(session_request()).await.expect("Session");
    let mut events = session.subscribe();
    client.replace_tool("after-reconnect");

    session.reconnect(&server_id).await.expect("reconnect");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = session.snapshot();
            if snapshot.servers[0].state == McpServerState::Ready
                && snapshot.catalog.tools.first().is_some_and(|tool| {
                    tool.reference.remote_name == "after-reconnect"
                })
            {
                break;
            }
            events.recv().await.expect("reconnect event");
        }
    })
    .await
    .expect("reconnect publication timeout");
}

/// Session shutdown cancels a catalog refresh instead of waiting for a silent Server forever.
#[tokio::test]
async fn shutdown_cancels_pending_catalog_refresh() {
    let server_id = McpServerId::try_from("dynamic").expect("Server id");
    let client = Arc::new(DynamicClient::new(server_id));
    let factory = SessionMcpFactory::new(
        vec![runtime_server()],
        Arc::new(DynamicConnector {
            client: Arc::clone(&client),
        }),
    );
    let session = factory.create(session_request()).await.expect("Session");
    client.hang_catalog_refresh();
    client.notify_tools_changed();
    tokio::time::timeout(
        Duration::from_millis(250),
        client.discovery_started.notified(),
    )
    .await
    .expect("catalog refresh must start");

    tokio::time::timeout(Duration::from_millis(250), session.shutdown())
        .await
        .expect("shutdown must cancel catalog refresh")
        .expect("Session shutdown");
}

/// Shutdown waits for an in-progress reconnect lifecycle instead of corrupting its state.
#[tokio::test]
async fn shutdown_serializes_with_reconnect() {
    let server_id = McpServerId::try_from("dynamic").expect("Server id");
    let client = Arc::new(DynamicClient::new(server_id.clone()));
    let server = McpServerRuntime::bootstrap(
        runtime_server(),
        Arc::new(DynamicConnector {
            client: Arc::clone(&client),
        }),
        SessionId::try_from("lifecycle-session").expect("Session id"),
        Arc::new(RejectingHost),
        CancellationToken::new(),
    )
    .await;
    client.hang_catalog_refresh();
    let reconnect_server = Arc::clone(&server);
    let reconnect =
        tokio::spawn(async move { reconnect_server.reconnect().await });
    tokio::time::timeout(
        Duration::from_millis(250),
        client.discovery_started.notified(),
    )
    .await
    .expect("reconnect discovery must start");
    let shutdown_server = Arc::clone(&server);
    let shutdown =
        tokio::spawn(async move { shutdown_server.shutdown().await });
    tokio::task::yield_now().await;
    client.release_catalog_refresh();

    reconnect
        .await
        .expect("reconnect task")
        .expect("reconnect must finish before shutdown");
    shutdown.await.expect("shutdown task");
    assert_eq!(server.status().state, McpServerState::Stopped);
}
