use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    McpAgentTool, McpClient, McpConnector, McpError, McpHost, McpMrtrPolicy,
    McpServerRuntime, McpStdioTransport, McpTransport, RuntimeMcpServer,
};
use protocol::{
    ContentBlock, McpCatalog, McpCompletionRequest, McpCompletionResult,
    McpHostRequest, McpHostResponse, McpPromptRequest, McpPromptResult,
    McpProtocolVersion, McpResourceRequest, McpResourceResult,
    McpServerCapabilities, McpServerId, McpServerImplementation, McpToolInfo,
    McpToolRef, McpToolRequest, McpToolResult, SessionId, ToolCall, ToolCallId,
    TurnId,
};
use tokio_util::sync::CancellationToken;
use tools::{AgentTool, DiscardToolUpdates, ToolError, ToolExecutionContext};

/// Fake connector that exposes one deterministic Tool client.
struct ToolConnector {
    result: McpToolResult,
    delay: Duration,
}

/// Fake client used to verify routing and lossless result conversion.
struct ToolClient {
    server_id: McpServerId,
    capabilities: McpServerCapabilities,
    result: McpToolResult,
    delay: Duration,
}

#[async_trait]
impl McpConnector for ToolConnector {
    /// Creates one client carrying this scenario's result and delay.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
        _session_id: &SessionId,
        _host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        Ok(Arc::new(ToolClient {
            server_id: server.server_id.clone(),
            capabilities: McpServerCapabilities::builder().tools(true).build(),
            result: self.result.clone(),
            delay: self.delay,
        }))
    }
}

/// Rejects Host callbacks because Tool adapter tests exercise only client requests.
struct RejectingHost;

#[async_trait]
impl McpHost for RejectingHost {
    /// Rejects any unexpected Server-to-client request.
    async fn handle(
        &self,
        _request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        Err(McpError::Host("unexpected Host callback".to_string()))
    }
}

#[async_trait]
impl McpClient for ToolClient {
    /// Returns this fake client's structured route owner.
    fn server_id(&self) -> &McpServerId {
        &self.server_id
    }

    /// Uses the Legacy lifecycle for this transport-independent adapter test.
    fn protocol(&self) -> McpProtocolVersion {
        McpProtocolVersion::V2025_11_25
    }

    /// Omits optional implementation metadata.
    fn server_info(&self) -> Option<&McpServerImplementation> {
        None
    }

    /// Declares Tool support.
    fn capabilities(&self) -> &McpServerCapabilities {
        &self.capabilities
    }

    /// Publishes the Tool definition used by the adapter.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        Ok(McpCatalog::builder().tools(vec![tool_info()]).build())
    }

    /// Returns the configured rich result after the configured delay.
    async fn call_tool(
        &self,
        request: McpToolRequest,
        control: mcp::McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        assert_eq!(request.reference.server_id, self.server_id);
        assert_eq!(control.context.server_id, self.server_id);
        assert_eq!(control.context.session_id.as_str(), "session-1");
        assert_eq!(control.context.turn_id.as_str(), "turn-1");
        assert_eq!(control.context.trace_id.as_str(), "trace-1");
        assert!(control.context.requested_at_ms.get() > 0);
        tokio::select! {
            _ = control.cancellation.cancelled() => {
                Err(McpError::RequestCancelled(self.server_id.clone()))
            }
            _ = tokio::time::sleep(control.idle_timeout) => {
                Err(McpError::RequestTimeout(self.server_id.clone()))
            }
            _ = tokio::time::sleep(self.delay) => Ok(self.result.clone()),
        }
    }

    /// Prompt retrieval is outside this adapter test.
    async fn get_prompt(
        &self,
        _request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        unreachable!("Prompt retrieval is not exercised")
    }

    /// Resource reads are outside this adapter test.
    async fn read_resource(
        &self,
        _request: McpResourceRequest,
    ) -> Result<McpResourceResult, McpError> {
        unreachable!("Resource reads are not exercised")
    }

    /// Completion is outside this adapter test.
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

/// Builds the stable namespaced Tool definition used by every scenario.
fn tool_info() -> McpToolInfo {
    let reference = McpToolRef {
        server_id: McpServerId::try_from("tool-server").expect("Server id"),
        remote_name: "inspect".to_string(),
    };
    McpToolInfo::builder()
        .reference(reference.clone())
        .public_name(reference.public_name())
        .description(Some("Inspect structured input".to_string()))
        .input_schema(serde_json::json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"],
            "additionalProperties": false
        }))
        .build()
}

/// Bootstraps an adapter through the same Server runtime used in production.
async fn adapter(
    result: McpToolResult,
    delay: Duration,
    request_timeout: Duration,
) -> McpAgentTool {
    let server = RuntimeMcpServer::builder()
        .server_id(McpServerId::try_from("tool-server").expect("Server id"))
        .enabled(true)
        .protocol(McpProtocolVersion::V2025_11_25)
        .startup_timeout(Duration::from_secs(1))
        .request_timeout(request_timeout)
        .mrtr(McpMrtrPolicy {
            max_rounds: NonZeroU32::new(2).expect("non-zero"),
            total_timeout: Duration::from_secs(1),
        })
        .transport(McpTransport::Stdio(Box::new(McpStdioTransport {
            command: "unused".to_string(),
            args: Vec::new(),
            env: Default::default(),
        })))
        .build();
    let runtime = McpServerRuntime::bootstrap(
        server,
        Arc::new(ToolConnector { result, delay }),
        SessionId::try_from("session-tool-runtime").expect("Session id"),
        Arc::new(RejectingHost),
        CancellationToken::new(),
    )
    .await;
    McpAgentTool::new(tool_info(), runtime, 1)
}

/// Builds one correlated Tool call and execution context.
fn invocation(
    arguments: serde_json::Value,
) -> (ToolCall, ToolExecutionContext) {
    let call = ToolCall {
        tool_call_id: ToolCallId::try_from("call-1").expect("Tool call id"),
        name: tool_info().public_name,
        arguments,
    };
    let context = ToolExecutionContext::builder()
        .session_id(SessionId::try_from("session-1").expect("Session id"))
        .turn_id(TurnId::try_from("turn-1").expect("Turn id"))
        .trace_id(protocol::TraceId::try_from("trace-1").expect("Trace id"))
        .cwd(PathBuf::from("/workspace"))
        .cancellation(CancellationToken::new())
        .updates(Arc::new(DiscardToolUpdates))
        .build();
    (call, context)
}

#[tokio::test]
async fn preserves_rich_content_and_structured_content() {
    let adapter = adapter(
        McpToolResult {
            content: vec![
                ContentBlock::Text {
                    text: "visible".to_string(),
                },
                ContentBlock::Audio {
                    data: "YXVkaW8=".to_string(),
                    mime_type: "audio/wav".to_string(),
                },
            ],
            structured_content: Some(serde_json::json!({ "count": 2 })),
            is_error: true,
        },
        Duration::ZERO,
        Duration::from_secs(1),
    )
    .await;
    let (call, context) = invocation(serde_json::json!({ "value": "ok" }));

    let result = adapter.execute(call, &context).await.expect("Tool result");

    assert!(result.is_error);
    assert_eq!(result.blocks.len(), 3);
    assert!(matches!(
        result.blocks.last(),
        Some(ContentBlock::Structured { value }) if *value == serde_json::json!({ "count": 2 })
    ));
}

#[tokio::test]
async fn rejects_non_object_and_schema_invalid_arguments_before_remote_call() {
    let adapter = adapter(
        McpToolResult {
            content: Vec::new(),
            structured_content: None,
            is_error: false,
        },
        Duration::ZERO,
        Duration::from_secs(1),
    )
    .await;

    for arguments in [serde_json::json!([]), serde_json::json!({ "value": 1 })]
    {
        let (call, context) = invocation(arguments);
        let error = adapter
            .execute(call, &context)
            .await
            .expect_err("invalid args");
        assert!(matches!(error, ToolError::InvalidArguments { .. }));
    }
}

#[tokio::test]
async fn observes_turn_cancellation_and_request_timeout() {
    let result = McpToolResult {
        content: Vec::new(),
        structured_content: None,
        is_error: false,
    };
    let cancelled_adapter = adapter(
        result.clone(),
        Duration::from_secs(10),
        Duration::from_secs(1),
    )
    .await;
    let (call, context) = invocation(serde_json::json!({ "value": "ok" }));
    context.cancellation.cancel();
    assert_eq!(
        cancelled_adapter
            .execute(call, &context)
            .await
            .expect_err("cancelled"),
        ToolError::Cancelled
    );

    let timeout_adapter =
        adapter(result, Duration::from_secs(10), Duration::from_millis(20))
            .await;
    let (call, context) = invocation(serde_json::json!({ "value": "ok" }));
    let error = timeout_adapter
        .execute(call, &context)
        .await
        .expect_err("timeout");
    assert!(matches!(error, ToolError::Execution { .. }));
}
