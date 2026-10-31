use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    McpClient, McpConnector, McpError, McpFactory, McpHost, McpSessionRequest,
    RuntimeMcpServer, SessionMcpFactory,
};
use protocol::{
    McpCatalog, McpCompletionRequest, McpCompletionResult, McpPromptRequest,
    McpPromptResult, McpProtocolVersion, McpResourceRequest, McpResourceResult,
    McpServerCapabilities, McpServerId, McpServerImplementation, McpToolInfo,
    McpToolRef, McpToolRequest, McpToolResult, ModelFinal, ModelProfile,
    ModelRequest, ModelStreamEvent, SessionId, StopReason,
};
use store::SessionCreateOptions;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Mutable remote catalog with no transport dependency in this startup-order regression.
struct CatalogClient {
    server_id: McpServerId,
    remote_name: Mutex<String>,
    events: broadcast::Sender<mcp::McpClientEvent>,
}

#[async_trait]
impl McpClient for CatalogClient {
    /// Returns the configured route identity.
    fn server_id(&self) -> &McpServerId {
        &self.server_id
    }
    /// Supplies a supported negotiated revision.
    fn protocol(&self) -> McpProtocolVersion {
        McpProtocolVersion::V2025_11_25
    }
    /// Omits optional implementation identity.
    fn server_info(&self) -> Option<&McpServerImplementation> {
        None
    }
    /// Allows the production runtime to subscribe to catalog invalidation.
    fn capabilities(&self) -> &McpServerCapabilities {
        static CAPABILITIES: std::sync::LazyLock<McpServerCapabilities> =
            std::sync::LazyLock::new(|| {
                McpServerCapabilities::builder()
                    .tools(true)
                    .tools_list_changed(true)
                    .build()
            });
        &CAPABILITIES
    }
    /// Returns the actual current remote name for normalization and publication.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        let reference = McpToolRef {
            server_id: self.server_id.clone(),
            remote_name: self.remote_name.lock().expect("name").clone(),
        };
        Ok(McpCatalog::builder()
            .tools(vec![
                McpToolInfo::builder()
                    .public_name(reference.public_name())
                    .reference(reference)
                    .input_schema(serde_json::json!({"type":"object"}))
                    .build(),
            ])
            .build())
    }
    /// Exposes the same invalidation boundary as a live MCP client.
    fn subscribe_changes(
        &self,
    ) -> Option<broadcast::Receiver<mcp::McpClientEvent>> {
        Some(self.events.subscribe())
    }
    /// Catalog projection must not execute remote tools.
    async fn call_tool(
        &self,
        _request: McpToolRequest,
        _control: mcp::McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        Err(McpError::Host("unexpected tool call".into()))
    }
    /// Prompt retrieval is outside this catalog regression.
    async fn get_prompt(
        &self,
        _request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        Err(McpError::Host("unexpected prompt".into()))
    }
    /// Resource reads are outside this catalog regression.
    async fn read_resource(
        &self,
        _request: McpResourceRequest,
        _cancel: &CancellationToken,
    ) -> Result<McpResourceResult, McpError> {
        Err(McpError::Host("unexpected resource".into()))
    }
    /// Completion is outside this catalog regression.
    async fn complete(
        &self,
        _request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        Err(McpError::Host("unexpected completion".into()))
    }
    /// The in-memory remote owns no transport resources.
    async fn shutdown(&self) -> Result<(), McpError> {
        Ok(())
    }
}

/// Connects the production server lifecycle to the controlled remote catalog.
struct CatalogConnector {
    client: Arc<CatalogClient>,
}
#[async_trait]
impl McpConnector for CatalogConnector {
    /// Shares the mutable catalog for the configured server.
    async fn connect(
        &self,
        _server: &RuntimeMcpServer,
        _session: &SessionId,
        _host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        Ok(Arc::clone(&self.client) as Arc<dyn McpClient>)
    }
}
/// Schedules exactly one remote change inside the Kernel startup gap.
struct Gate {
    client: Arc<CatalogClient>,
    events: Mutex<Option<broadcast::Receiver<mcp::McpSessionEvent>>>,
    fired: AtomicBool,
}
impl Gate {
    /// Waits for the native Session snapshot to publish before allowing Kernel construction to continue.
    fn publish_in_gap(&self) {
        let mut events = self.events.lock().expect("events");
        if events.is_none() || self.fired.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.client.remote_name.lock().expect("name") = "new".into();
        let _ = self.client.events.send(mcp::McpClientEvent::ToolsChanged);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match events.as_mut().expect("receiver").try_recv() {
                Ok(event) if event.change == mcp::McpSessionChange::Catalog => {
                    return;
                }
                _ => {}
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native catalog update"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
/// Keeps an independent receiver for the native publication barrier.
struct Factory {
    inner: SessionMcpFactory,
    gate: Arc<Gate>,
}
#[async_trait]
impl McpFactory for Factory {
    /// Arms the startup gap after native bootstrap.
    async fn create(
        &self,
        request: McpSessionRequest,
    ) -> Result<mcp::McpSession, McpError> {
        let session = self.inner.create(request).await?;
        *self.gate.events.lock().expect("events") = Some(session.subscribe());
        Ok(session)
    }
}
/// Injects one catalog update during local indexing, retaining the real Store for the whole test.
struct StartupTools {
    gate: Arc<Gate>,
}

impl tools::ToolFactory for StartupTools {
    /// Installs the deferred definition used as a precise startup ordering barrier.
    fn create(&self) -> Result<tools::ToolRegistry, tools::ToolError> {
        let mut registry = tools::ToolRegistry::default();
        registry.register(Arc::new(Self {
            gate: Arc::clone(&self.gate),
        }))?;
        Ok(registry)
    }
}

#[async_trait]
impl tools::AgentTool for StartupTools {
    /// Pauses indexing only after the native MCP snapshot is subscribed and can advance.
    fn definition(&self) -> protocol::ToolDefinition {
        self.gate.publish_in_gap();
        protocol::ToolDefinition {
            name: "startup_gate".into(),
            description: "Startup barrier".into(),
            parameters: serde_json::json!({"type":"object"}),
        }
    }

    /// Defers the local tool so startup indexing reads its definition without invoking it.
    fn metadata(&self) -> &protocol::ToolMetadata {
        static METADATA: std::sync::LazyLock<protocol::ToolMetadata> =
            std::sync::LazyLock::new(|| {
                protocol::ToolMetadata::builder()
                    .exposure(protocol::ToolExposure::Deferred)
                    .build()
            });
        &METADATA
    }

    /// Startup projection must not execute the barrier tool.
    async fn execute(
        &self,
        _call: protocol::ToolCall,
        _context: &tools::ToolExecutionContext,
    ) -> Result<protocol::ToolResult, tools::ToolError> {
        Err(tools::ToolError::Execution {
            tool: "startup_gate".into(),
            message: "unexpected execution".into(),
        })
    }
}

struct RecordingModel {
    profile: ModelProfile,
    requests: Mutex<Vec<Vec<String>>>,
}
#[async_trait]
impl kernel::Model for RecordingModel {
    /// Returns the synthetic profile for a protocol-level integration probe.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }
    /// No provider traffic is needed to inspect the exact Kernel request.
    async fn preflight(&self) -> Result<(), kernel::ModelError> {
        Ok(())
    }
    /// Records declarations at the actual model boundary.
    async fn stream(
        &self,
        r: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<kernel::ModelStream, kernel::ModelError> {
        self.requests
            .lock()
            .expect("requests")
            .push(r.tools.into_iter().map(|tool| tool.name).collect());
        Ok(Box::pin(futures::stream::iter([Ok(
            ModelStreamEvent::Finished(
                ModelFinal::builder()
                    .stop_reason(StopReason::EndTurn)
                    .raw_stop_reason(None)
                    .usage(protocol::ModelUsage::from(
                        protocol::Usage::new()
                            .input_tokens(0)
                            .output_tokens(0)
                            .cached_input_tokens(0)
                            .cache_creation_input_tokens(0)
                            .total_tokens(0),
                    ))
                    .build(),
            ),
        )])))
    }
}
struct Models(Arc<RecordingModel>);
impl kernel::ModelFactory for Models {
    /// Returns the shared recorder.
    fn create(&self) -> Result<Arc<dyn kernel::Model>, kernel::ModelError> {
        Ok(Arc::clone(&self.0) as Arc<dyn kernel::Model>)
    }
}
struct Sink;
#[async_trait]
impl kernel::EventSink for Sink {
    /// Discards events after Kernel has persisted them normally.
    async fn emit(
        &self,
        _event: protocol::AgentEvent,
    ) -> Result<(), kernel::SinkError> {
        Ok(())
    }
}
/// One change during startup must reach the first model request without another notification.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_catalog_change_reaches_first_request() {
    let root = tempfile::tempdir().expect("root");
    let client = Arc::new(CatalogClient {
        server_id: McpServerId::try_from("dynamic").expect("id"),
        remote_name: Mutex::new("old".into()),
        events: broadcast::channel(16).0,
    });
    let gate = Arc::new(Gate {
        client: Arc::clone(&client),
        events: Mutex::new(None),
        fired: AtomicBool::new(false),
    });
    let model = Arc::new(RecordingModel {
        profile: ModelProfile::builder()
            .provider_id("fixture".into())
            .model_id("catalog-startup".into())
            .display_name("Catalog startup".into())
            .context_tokens(128000)
            .max_output_tokens(1024)
            .build(),
        requests: Mutex::new(Vec::new()),
    });
    let kernel = kernel::KernelFactory::builder()
        .model_factory(Arc::new(Models(Arc::clone(&model))))
        .tool_factory(Arc::new(StartupTools {
            gate: Arc::clone(&gate),
        }))
        .store_factory(Arc::new(store::JsonlStoreFactory::new(
            root.path(),
            Arc::new(store::SystemClock),
        )))
        .prompt_factory(Arc::new(prompt::FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(
            extension::StaticExtensionFactory::default(),
        ))
        .mcp_factory(Some(Arc::new(Factory {
            inner: SessionMcpFactory::new(
                vec![
                    RuntimeMcpServer::try_from(
                        config::McpServerConfig::builder()
                            .name("dynamic".into())
                            .command("unused".into())
                            .build(),
                    )
                    .expect("config"),
                ],
                Arc::new(CatalogConnector { client }),
            ),
            gate,
        })))
        .clock(Arc::new(store::SystemClock))
        .id_generator(Arc::new(kernel::NanoidIdGenerator))
        .build()
        .build()
        .expect("Kernel");
    let id = SessionId::try_from("startup-race").expect("id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: id.clone(),
            cwd: root.path().into(),
            parent_session_id: None,
        })
        .await
        .expect("session");
    let catalog: Vec<_> = kernel
        .mcp_status(&id)
        .expect("snapshot")
        .catalog
        .tools
        .into_iter()
        .map(|t| t.public_name)
        .collect();
    kernel
        .run(
            protocol::RunRequest {
                session_id: id.clone(),
                input: "inspect declarations".into(),
            },
            Arc::new(Sink),
        )
        .await
        .expect("run");
    let declarations = model
        .requests
        .lock()
        .expect("requests")
        .first()
        .expect("request")
        .iter()
        .filter(|name| name.starts_with("mcp__"))
        .cloned()
        .collect::<Vec<_>>();
    kernel.close_session(&id).await.expect("close");
    assert_eq!(catalog, vec!["mcp__dynamic__new"]);
    assert_eq!(
        declarations,
        vec!["mcp__dynamic__new"],
        "startup catalog event was lost"
    );
}
