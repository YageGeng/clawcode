use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    McpAuthentication, McpClient, McpError, McpHost, McpHttpHeaders,
    McpHttpUrl, McpMrtrPolicy, McpProgress, McpProgressSink, McpRequestControl,
    McpStdioTransport, McpStreamableHttpTransport, McpTransport,
    RuntimeMcpServer,
};
use protocol::{
    McpCompletionRequest, McpCompletionTarget, McpHostRequest, McpHostResponse,
    McpPromptRequest, McpProtocolVersion, McpRequestContext,
    McpResourceRequest, McpServerId, McpToolRequest, SessionId, TimestampMs,
    TraceId, TurnId,
};
use tokio_util::sync::CancellationToken;

#[path = "../../examples/reference_server/mod.rs"]
pub mod reference_server;

/// Test Host that rejects callbacks because baseline interop calls are client-initiated.
pub struct RejectingHost;

#[async_trait]
impl McpHost for RejectingHost {
    /// Rejects unexpected Server-to-client requests during baseline interop coverage.
    async fn handle(
        &self,
        _request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        Err(McpError::Host("unexpected Host callback".to_string()))
    }
}

/// Progress sink used when the reference Tool completes synchronously.
struct DiscardProgress;

impl McpProgressSink for DiscardProgress {
    /// Discards progress because the reference Tool has no intermediate states.
    fn publish(&self, _progress: McpProgress) {}
}

/// Namespace for constructing exact-protocol interop configurations.
pub struct InteropConfig;

#[allow(
    dead_code,
    reason = "each integration-test crate compiles shared support independently"
)]
impl InteropConfig {
    /// Builds the Legacy stdio configuration for a child test process.
    pub fn legacy(
        command: String,
        args: Vec<String>,
        env: std::collections::HashMap<String, String>,
    ) -> RuntimeMcpServer {
        Self::runtime(
            "legacy-reference",
            McpProtocolVersion::V2025_11_25,
            McpTransport::Stdio(Box::new(McpStdioTransport {
                command,
                args,
                env,
            })),
        )
    }

    /// Builds the Modern Streamable HTTP configuration for a live listener.
    pub fn modern(url: String) -> RuntimeMcpServer {
        Self::runtime(
            "modern-reference",
            McpProtocolVersion::V2026_07_28,
            McpTransport::StreamableHttp(Box::new(
                McpStreamableHttpTransport {
                    url: McpHttpUrl::try_from(url).expect("valid HTTP URL"),
                    headers: McpHttpHeaders::default(),
                    authentication: McpAuthentication::None,
                },
            )),
        )
    }

    /// Applies shared bounded request policy to one exact transport definition.
    fn runtime(
        server_id: &str,
        protocol: McpProtocolVersion,
        transport: McpTransport,
    ) -> RuntimeMcpServer {
        RuntimeMcpServer::builder()
            .server_id(McpServerId::try_from(server_id).expect("Server id"))
            .enabled(true)
            .protocol(protocol)
            .startup_timeout(Duration::from_secs(10))
            .request_timeout(Duration::from_secs(10))
            .mrtr(McpMrtrPolicy {
                max_rounds: NonZeroU32::new(4).expect("non-zero rounds"),
                total_timeout: Duration::from_secs(30),
            })
            .transport(transport)
            .build()
    }
}

/// Exercises all baseline catalog families and their corresponding requests.
pub async fn verify_client(
    client: &dyn McpClient,
    server_id: &McpServerId,
    protocol: McpProtocolVersion,
) -> Result<(), McpError> {
    assert_eq!(client.protocol(), protocol);
    assert_eq!(client.server_id(), server_id);
    let catalog = client.discover().await?;
    assert_eq!(catalog.tools.len(), 1);
    assert_eq!(catalog.prompts.len(), 1);
    assert_eq!(catalog.resources.len(), 1);
    assert_eq!(catalog.resource_templates.len(), 1);

    let tool = catalog
        .tools
        .first()
        .expect("reference Tool")
        .reference
        .clone();
    let tool_result = client
        .call_tool(
            McpToolRequest {
                reference: tool,
                arguments: serde_json::Map::from_iter([(
                    "value".to_string(),
                    serde_json::Value::String("interop".to_string()),
                )]),
            },
            McpRequestControl::builder()
                .context(
                    McpRequestContext::builder()
                        .server_id(server_id.clone())
                        .session_id(
                            SessionId::try_from("interop-session")
                                .expect("Session id"),
                        )
                        .turn_id(
                            TurnId::try_from("interop-turn").expect("Turn id"),
                        )
                        .trace_id(
                            TraceId::try_from("interop-trace")
                                .expect("Trace id"),
                        )
                        .requested_at_ms(TimestampMs::now())
                        .build(),
                )
                .cancellation(CancellationToken::new())
                .lifecycle(CancellationToken::new())
                .idle_timeout(Duration::from_secs(10))
                .total_timeout(Duration::from_secs(30))
                .max_rounds(NonZeroU32::new(4).expect("non-zero rounds"))
                .progress(Arc::new(DiscardProgress) as Arc<dyn McpProgressSink>)
                .build(),
        )
        .await?;
    assert!(!tool_result.is_error);
    assert_eq!(
        tool_result.structured_content,
        Some(serde_json::json!({ "value": "interop" }))
    );

    let prompt = catalog
        .prompts
        .first()
        .expect("reference Prompt")
        .reference
        .clone();
    let prompt_result = client
        .get_prompt(McpPromptRequest {
            reference: prompt.clone(),
            arguments: BTreeMap::from([(
                "topic".to_string(),
                "Rust".to_string(),
            )]),
        })
        .await?;
    assert_eq!(prompt_result.messages.len(), 1);

    let resource_result = client
        .read_resource(McpResourceRequest {
            reference: catalog
                .resources
                .first()
                .expect("reference Resource")
                .reference
                .clone(),
        })
        .await?;
    assert_eq!(resource_result.contents.len(), 1);

    let completion = client
        .complete(
            McpCompletionRequest::builder()
                .target(McpCompletionTarget::Prompt(prompt))
                .argument_name("topic".to_string())
                .argument_value("Ru".to_string())
                .build(),
        )
        .await?;
    assert_eq!(completion.values, vec!["Rust", "Runtime"]);
    assert!(!completion.has_more);
    Ok(())
}
