use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    McpClient, McpClientEvent, McpConnector, McpError, McpFactory, McpHost,
    McpServerRuntime, McpSessionRequest, McpStdioTransport, McpTransport,
    RuntimeMcpServer, SessionMcpFactory,
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

    /// Keeps the remote Tool healthy even when catalog discovery is failing.
    async fn call_tool(
        &self,
        _request: McpToolRequest,
        _control: mcp::McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        Ok(McpToolResult {
            content: vec![protocol::ContentBlock::Text {
                text: "available".to_string(),
            }],
            structured_content: None,
            is_error: false,
        })
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
        _cancellation: &tokio_util::sync::CancellationToken,
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
        .protocol_policy(protocol::McpProtocolPolicy::Auto)
        .startup_timeout(Duration::from_secs(1))
        .request_timeout(Duration::from_secs(1))
        .request_total_timeout(Duration::from_secs(2))
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

/// Catalog failures retain the last complete revision while successful refreshes advance it.
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

/// Reconnect withdraws only its owning server before publishing the new negotiated catalog.
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

/// Ignores progress from the deterministic healthy remote Tool.
struct NoProgress;

impl mcp::McpProgressSink for NoProgress {
    /// Final results are sufficient for this callability regression.
    fn publish(&self, _progress: mcp::McpProgress) {}
}

/// Catalog degradation retains usable Tools, while shutdown still rejects new calls.
#[tokio::test]
async fn degraded_catalog_keeps_existing_tools_callable() {
    let server_id = McpServerId::try_from("dynamic").expect("Server id");
    let client = Arc::new(DynamicClient::new(server_id.clone()));
    let server = McpServerRuntime::bootstrap(
        runtime_server(),
        Arc::new(DynamicConnector {
            client: Arc::clone(&client),
        }),
        SessionId::try_from("degraded-session").expect("Session id"),
        Arc::new(RejectingHost),
        CancellationToken::new(),
    )
    .await;
    let context = protocol::McpRequestContext::builder()
        .server_id(server_id.clone())
        .session_id(
            SessionId::try_from("degraded-session").expect("Session id"),
        )
        .turn_id(protocol::TurnId::try_from("turn").expect("Turn id"))
        .trace_id(protocol::TraceId::try_from("trace").expect("Trace id"))
        .requested_at_ms(protocol::TimestampMs::now())
        .build();
    let request = McpToolRequest {
        reference: tool(&server_id, "old").reference,
        arguments: Default::default(),
    };
    let cancellation = CancellationToken::new();
    server
        .call_tool(
            request.clone(),
            context.clone(),
            &cancellation,
            Arc::new(NoProgress),
        )
        .await
        .expect("initial Tool call");
    client.fail_discovery.store(true, Ordering::SeqCst);
    client.notify_tools_changed();
    tokio::time::timeout(Duration::from_secs(1), async {
        while server.status().state != McpServerState::Degraded {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Server becomes degraded");
    assert_eq!(server.catalog().tools.len(), 1);
    let result = server
        .call_tool(
            request.clone(),
            context.clone(),
            &cancellation,
            Arc::new(NoProgress),
        )
        .await;
    server.shutdown().await;
    assert!(matches!(
        server
            .call_tool(request, context, &cancellation, Arc::new(NoProgress))
            .await,
        Err(McpError::ServerUnavailable {
            state: McpServerState::Stopped,
            ..
        })
    ));
    let result =
        result.expect("catalog failure must not disable the healthy client");
    assert_eq!(
        result.content,
        vec![protocol::ContentBlock::Text {
            text: "available".to_string()
        }]
    );
}

/// Resolves independently changing Servers without sharing their identities.
struct MultiConnector(Vec<Arc<DynamicClient>>);

/// Namespace separators can collide across valid server ids; no ambiguous aggregate is published.
#[tokio::test]
async fn cross_server_name_collision_rejects_ambiguous_catalogs() {
    let mut clients = Vec::new();
    let mut configs = Vec::new();
    for (name, remote) in [("a", "_x"), ("a_", "x")] {
        let server_id = McpServerId::try_from(name).expect("id");
        let client = Arc::new(DynamicClient::new(server_id.clone()));
        client.replace_tool(remote);
        let mut config = runtime_server();
        config.server_id = server_id;
        configs.push(config);
        clients.push(client);
    }
    let factory =
        SessionMcpFactory::new(configs, Arc::new(MultiConnector(clients)));
    let session =
        Arc::new(factory.create(session_request()).await.expect("session"));
    assert!(
        session.snapshot().catalog.tools.is_empty(),
        "ambiguous public names must not enter the aggregate catalog"
    );
    assert!(
        session
            .snapshot()
            .servers
            .iter()
            .all(|server| server.state == McpServerState::Failed)
    );
    session.tool_registry().expect("unambiguous registry");
    session
        .read_resource(
            McpResourceRequest {
                reference: protocol::McpResourceRef {
                    server_id: McpServerId::try_from("a").expect("id"),
                    remote_uri: "opaque://resource".into(),
                },
            },
            &CancellationToken::new(),
        )
        .await
        .expect_err("failed aggregate must not start a resource read");
    session.shutdown().await.expect("shutdown");
}

/// A conflicting refresh preserves its previous catalog and recovers after the peer frees the name.
#[tokio::test]
async fn cross_server_refresh_preserves_last_good_catalog() {
    let first =
        Arc::new(DynamicClient::new(McpServerId::try_from("a").expect("id")));
    let second =
        Arc::new(DynamicClient::new(McpServerId::try_from("a_").expect("id")));
    first.replace_tool("safe");
    second.replace_tool("x");
    let configs = [&first, &second]
        .into_iter()
        .map(|client| {
            let mut config = runtime_server();
            config.server_id = client.server_id.clone();
            config
        })
        .collect();
    let factory = SessionMcpFactory::new(
        configs,
        Arc::new(MultiConnector(vec![
            Arc::clone(&first),
            Arc::clone(&second),
        ])),
    );
    let session =
        Arc::new(factory.create(session_request()).await.expect("session"));
    let original = session.snapshot().catalog.clone();
    first.replace_tool("_x");
    first.notify_tools_changed();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !session
            .snapshot()
            .servers
            .iter()
            .any(|status| status.state == McpServerState::Degraded)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reject conflict");
    assert_eq!(session.snapshot().catalog, original);
    session
        .tool_registry()
        .expect("previous catalog remains usable");
    second.replace_tool("new-safe");
    second.notify_tools_changed();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let view = session.snapshot();
            if view
                .servers
                .iter()
                .all(|status| status.state == McpServerState::Ready)
                && view
                    .catalog
                    .tools
                    .iter()
                    .any(|tool| tool.reference.remote_name == "_x")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recover conflict");
    session.shutdown().await.expect("shutdown");
}

#[async_trait]
impl McpConnector for MultiConnector {
    /// Selects the configured remote test Server while retaining the real lifecycle runtime.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
        _session_id: &SessionId,
        _host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        let client = self
            .0
            .iter()
            .find(|client| client.server_id == server.server_id)
            .expect("configured Server");
        Ok(Arc::clone(client) as Arc<dyn McpClient>)
    }
}

/// Concurrent changes publish coherent catalog counts as well as monotonic revisions.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_catalog_publication_preserves_revision_order() {
    let mut clients = Vec::new();
    let mut configs = Vec::new();
    for index in 0..6 {
        let server_id = McpServerId::try_from(format!("server-{index}"))
            .expect("Server id");
        let client = Arc::new(DynamicClient::new(server_id.clone()));
        // A non-trivial catalog exposes interleavings between aggregation and publication.
        let tools = (0..64)
            .map(|index| {
                let mut info = tool(&server_id, &format!("tool-{index}"));
                info.description = Some("description ".repeat(350));
                info
            })
            .collect();
        *client.catalog.write().expect("catalog lock") =
            McpCatalog::builder().tools(tools).build();
        let mut config = runtime_server();
        config.server_id = server_id;
        configs.push(config);
        clients.push(client);
    }
    let factory = SessionMcpFactory::new(
        configs,
        Arc::new(MultiConnector(clients.iter().map(Arc::clone).collect())),
    );
    let session =
        Arc::new(factory.create(session_request()).await.expect("Session"));
    let mut events = session.subscribe();
    let observed_session = Arc::clone(&session);
    let observer = tokio::spawn(async move {
        let mut last_event = 1;
        let mut last_snapshot = 1;
        let mut regression = None;
        let mut inconsistent = None;
        loop {
            let event = match events.recv().await {
                Ok(event) => Some(event),
                Err(broadcast::error::RecvError::Lagged(_)) => None,
                Err(broadcast::error::RecvError::Closed) => {
                    panic!("missing shutdown event")
                }
            };
            let snapshot = observed_session.snapshot();
            if snapshot.revision < last_snapshot {
                regression.get_or_insert((last_snapshot, snapshot.revision));
            }
            last_snapshot = snapshot.revision;
            for server in &snapshot.servers {
                let tools = snapshot
                    .catalog
                    .tools
                    .iter()
                    .filter(|tool| tool.reference.server_id == server.server_id)
                    .count();
                if server.counts.tools != tools {
                    inconsistent.get_or_insert((
                        snapshot.revision,
                        server.counts.tools,
                        tools,
                    ));
                }
            }
            if let Some(event) = event {
                if event.revision <= last_event {
                    regression.get_or_insert((last_event, event.revision));
                }
                last_event = event.revision;
                if event.change == mcp::McpSessionChange::Shutdown {
                    break;
                }
            }
        }
        (regression, inconsistent, last_event)
    });
    let writers = clients.into_iter().map(|client| {
        tokio::spawn(async move {
            let full_catalog =
                client.catalog.read().expect("catalog lock").clone();
            for index in 0..60 {
                // Change actual contents so a status/catalog split cannot hide behind equal counts.
                *client.catalog.write().expect("catalog lock") =
                    if index % 2 == 0 {
                        full_catalog.clone()
                    } else {
                        McpCatalog::builder()
                            .tools(vec![tool(&client.server_id, "small")])
                            .build()
                    };
                client.notify_tools_changed();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    });
    for writer in futures::future::join_all(writers).await {
        writer.expect("notification producer");
    }
    session.shutdown().await.expect("Session shutdown");
    let (regression, inconsistent, last_event) =
        tokio::time::timeout(Duration::from_secs(5), observer)
            .await
            .expect("publication observer must stop")
            .expect("observer task");
    assert!(regression.is_none(), "revision regressed: {regression:?}");
    assert!(
        inconsistent.is_none(),
        "snapshot mixed catalog and status: {inconsistent:?}"
    );
    assert!(last_event > 2, "must observe updates before shutdown");
    assert_eq!(session.snapshot().revision, last_event);
}
/// Collisions reject the whole refresh and preserve the last usable catalog.
#[tokio::test]
async fn conflicting_catalog_preserves_previous_revision() {
    let server_id = McpServerId::try_from("dynamic").expect("id");
    let client = Arc::new(DynamicClient::new(server_id.clone()));
    let mut config = runtime_server();
    config.exposure = protocol::ToolExposure::Deferred;
    config
        .tool_exposure
        .insert("old".into(), protocol::ToolExposure::Direct);
    let server = McpServerRuntime::bootstrap(
        config,
        Arc::new(DynamicConnector {
            client: Arc::clone(&client),
        }),
        SessionId::try_from("collision").expect("session"),
        Arc::new(RejectingHost),
        CancellationToken::new(),
    )
    .await;
    let original = server.catalog();
    assert_eq!(
        original.tools[0].metadata.exposure,
        protocol::ToolExposure::Direct
    );
    let generated = tool(&server_id, "a.b");
    let literal = tool(
        &server_id,
        generated
            .public_name
            .strip_prefix("mcp__dynamic__")
            .expect("prefix"),
    );
    assert_eq!(generated.public_name, literal.public_name);
    for tools in [
        vec![generated.clone(), generated.clone()],
        vec![generated, literal],
    ] {
        *client.catalog.write().expect("lock") =
            McpCatalog::builder().tools(tools).build();
        client.notify_tools_changed();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if server.status().state == McpServerState::Degraded {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("rejected refresh");
        assert_eq!(server.catalog().as_ref(), original.as_ref());
    }
    server.shutdown().await;
}
