use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    McpClient, McpConnector, McpError, McpHost, McpMrtrPolicy,
    McpSessionRequest, McpStdioTransport, McpTransport, RuntimeMcpServer,
};
use protocol::{
    McpCatalog, McpCompletionRequest, McpCompletionResult, McpHostRequest,
    McpHostResponse, McpPromptRequest, McpPromptResult, McpProtocolVersion,
    McpResourceRequest, McpResourceResult, McpServerCapabilities, McpServerId,
    McpServerImplementation, McpToolInfo, McpToolRef, McpToolRequest,
    McpToolResult, SessionId,
};
use tokio_util::sync::CancellationToken;

/// Connector that exposes concurrency and deterministic Server-specific failures.
pub struct ScenarioConnector {
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl ScenarioConnector {
    /// Creates a connector with no startup calls in flight.
    pub fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        }
    }

    /// Returns the largest number of overlapping connection attempts observed.
    pub fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

/// Tool-only fake client used after deterministic connection setup.
struct ScenarioClient {
    server_id: McpServerId,
    discovery_fails: bool,
    discovery_hangs: bool,
    capabilities: McpServerCapabilities,
}

/// Host implementation that rejects requests because bootstrap must not invoke Host callbacks.
pub struct RejectingHost;

#[async_trait]
impl McpHost for RejectingHost {
    /// Rejects unexpected Host interaction during Session bootstrap.
    async fn handle(
        &self,
        _request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        Err(McpError::Host("unexpected Host request".to_string()))
    }
}

#[async_trait]
impl McpConnector for ScenarioConnector {
    /// Simulates concurrent startup, connection errors, mismatch, and discovery errors.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
        _session_id: &SessionId,
        _host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(80)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);

        match server.server_id.as_str() {
            "transport-failed" => Err(McpError::Transport {
                server_id: server.server_id.clone(),
                message: "connection refused".to_string(),
            }),
            "protocol-mismatch" => Err(McpError::ProtocolMismatch {
                server_id: server.server_id.clone(),
                expected: server.protocol,
                actual: "2099-01-01".to_string(),
            }),
            name => Ok(Arc::new(ScenarioClient {
                server_id: server.server_id.clone(),
                discovery_fails: name == "discovery-failed",
                discovery_hangs: name == "discovery-hangs",
                capabilities: McpServerCapabilities::builder()
                    .tools(true)
                    .build(),
            })),
        }
    }
}

#[async_trait]
impl McpClient for ScenarioClient {
    /// Returns the fake client's owning Server.
    fn server_id(&self) -> &McpServerId {
        &self.server_id
    }

    /// Returns the fixed Legacy protocol used by scenario configs.
    fn protocol(&self) -> McpProtocolVersion {
        McpProtocolVersion::V2025_11_25
    }

    /// Scenario clients omit implementation identity.
    fn server_info(&self) -> Option<&McpServerImplementation> {
        None
    }

    /// Returns the Tool-only capability gate.
    fn capabilities(&self) -> &McpServerCapabilities {
        &self.capabilities
    }

    /// Returns one Tool or a deterministic discovery failure.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        if self.discovery_hangs {
            return std::future::pending().await;
        }
        if self.discovery_fails {
            return Err(McpError::Service {
                server_id: self.server_id.clone(),
                message: "catalog unavailable".to_string(),
            });
        }
        let reference = McpToolRef {
            server_id: self.server_id.clone(),
            remote_name: "echo".to_string(),
        };
        Ok(McpCatalog::builder()
            .tools(vec![
                McpToolInfo::builder()
                    .public_name(reference.public_name())
                    .reference(reference)
                    .input_schema(serde_json::json!({ "type": "object" }))
                    .build(),
            ])
            .build())
    }

    /// Tool invocation is outside bootstrap scenario coverage.
    async fn call_tool(
        &self,
        _request: McpToolRequest,
        _control: mcp::McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        Err(McpError::Host("unexpected Tool call".to_string()))
    }

    /// Prompt retrieval is outside bootstrap scenario coverage.
    async fn get_prompt(
        &self,
        _request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        Err(McpError::Host("unexpected Prompt request".to_string()))
    }

    /// Resource reading is outside bootstrap scenario coverage.
    async fn read_resource(
        &self,
        _request: McpResourceRequest,
    ) -> Result<McpResourceResult, McpError> {
        Err(McpError::Host("unexpected Resource request".to_string()))
    }

    /// Completion is outside bootstrap scenario coverage.
    async fn complete(
        &self,
        _request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        Err(McpError::Host("unexpected Completion request".to_string()))
    }

    /// Fake shutdown has no transport work.
    async fn shutdown(&self) -> Result<(), McpError> {
        Ok(())
    }
}

/// Builds one deterministic Legacy stdio config for Session tests.
pub fn runtime_server(name: &str, enabled: bool) -> RuntimeMcpServer {
    RuntimeMcpServer::builder()
        .server_id(McpServerId::try_from(name).expect("valid Server id"))
        .enabled(enabled)
        .protocol(McpProtocolVersion::V2025_11_25)
        .startup_timeout(Duration::from_secs(5))
        .request_timeout(Duration::from_secs(5))
        .mrtr(McpMrtrPolicy {
            max_rounds: NonZeroU32::new(8).expect("non-zero rounds"),
            total_timeout: Duration::from_secs(30),
        })
        .transport(McpTransport::Stdio(Box::new(McpStdioTransport {
            command: "unused".to_string(),
            args: Vec::new(),
            env: Default::default(),
        })))
        .build()
}

/// Builds one complete Session creation request with independent shutdown ownership.
pub fn session_request() -> McpSessionRequest {
    McpSessionRequest::builder()
        .session_id(SessionId::try_from("session-mcp").expect("Session id"))
        .cwd(PathBuf::from("/workspace"))
        .host(Arc::new(RejectingHost) as Arc<dyn McpHost>)
        .shutdown(CancellationToken::new())
        .build()
}
