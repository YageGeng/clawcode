#![expect(
    deprecated,
    reason = "Legacy 2025-11-25 Host callbacks remain an explicitly supported protocol"
)]

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    LegacyClient, McpClient, McpError, McpHost, McpProgress, McpProgressSink,
    McpRequestControl,
};
use protocol::{
    ContentBlock, McpElicitationAction, McpElicitationMode,
    McpElicitationResult, McpHostRequest, McpHostResponse, McpRequestContext,
    McpRoot, McpRootsResult, McpSamplingResult, McpServerId, McpToolRef,
    McpToolRequest, Role, SessionId, StopReason, TimestampMs, TraceId, TurnId,
};
use rmcp::model::{
    CallToolResult, ClientCapabilities, ClientJsonRpcMessage, ClientRequest,
    ClientResult, CreateMessageRequest, CreateMessageRequestParams,
    ElicitRequest, ElicitRequestParams, ElicitationAction, Implementation,
    InitializeResult, ListRootsRequest, ListToolsResult, ProtocolVersion,
    RequestId, SamplingMessage, ServerCapabilities, ServerJsonRpcMessage,
    ServerRequest, ServerResult, Tool,
};
use rmcp::transport::{IntoTransport, Transport};
use tokio_util::sync::CancellationToken;

/// Records the typed Host request and returns one application-approved root.
struct CapturingHost(Mutex<Vec<McpHostRequest>>);

#[async_trait]
impl McpHost for CapturingHost {
    /// Captures one callback and returns the matching typed response.
    async fn handle(
        &self,
        request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        self.0
            .lock()
            .expect("Host request lock")
            .push(request.clone());
        match request {
            McpHostRequest::Roots(_) => {
                Ok(McpHostResponse::Roots(McpRootsResult {
                    roots: vec![McpRoot {
                        uri: "file:///workspace".to_string(),
                        name: Some("workspace".to_string()),
                    }],
                }))
            }
            McpHostRequest::Sampling(_) => Ok(McpHostResponse::Sampling(
                McpSamplingResult::builder()
                    .role(Role::Assistant)
                    .blocks(vec![ContentBlock::Text {
                        text: "sampled".to_string(),
                    }])
                    .stop_reason(StopReason::EndTurn)
                    .model("host-model".to_string())
                    .build(),
            )),
            McpHostRequest::Elicitation(_) => {
                Ok(McpHostResponse::Elicitation(McpElicitationResult {
                    action: McpElicitationAction::Accept,
                    content: Some(serde_json::json!({ "approved": true })),
                }))
            }
            McpHostRequest::Authorization(_) => {
                Err(McpError::Host("unexpected authorization".to_string()))
            }
        }
    }
}

/// Ignores progress because this Host association test has no progress stream.
struct NoProgress;

impl McpProgressSink for NoProgress {
    /// Discards an unexpected progress update.
    fn publish(&self, _progress: McpProgress) {}
}

/// A Legacy Tool-scoped Roots request inherits its exact Turn and Trace context.
#[tokio::test]
async fn legacy_roots_callback_inherits_tool_request_context() {
    let (server_transport, client_transport) = tokio::io::duplex(8_192);
    let mut server = IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        server_transport,
    );
    let server_task = tokio::spawn(async move {
        let ClientJsonRpcMessage::Request(initialize) =
            server.receive().await.expect("initialize request")
        else {
            panic!("expected initialize request");
        };
        let ClientRequest::InitializeRequest(request) = initialize.request
        else {
            panic!("expected initialize request");
        };
        assert_eq!(
            request.params.protocol_version,
            ProtocolVersion::V_2025_11_25
        );
        assert_eq!(
            request.params.capabilities,
            ClientCapabilities::builder()
                .enable_roots()
                .enable_sampling()
                .enable_elicitation()
                .build()
        );
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::InitializeResult(
                    InitializeResult::new(
                        ServerCapabilities::builder().enable_tools().build(),
                    )
                    .with_protocol_version(ProtocolVersion::V_2025_11_25)
                    .with_server_info(Implementation::new(
                        "host-reference",
                        "1.0.0",
                    )),
                ),
                initialize.id,
            ))
            .await
            .expect("initialize response");
        assert!(matches!(
            server.receive().await,
            Some(ClientJsonRpcMessage::Notification(_))
        ));

        let ClientJsonRpcMessage::Request(list) =
            server.receive().await.expect("Tool list request")
        else {
            panic!("expected Tool list request");
        };
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::ListToolsResult(ListToolsResult::with_all_items(
                    vec![Tool::new(
                        "host_tool",
                        "Requests Host roots",
                        serde_json::Map::new(),
                    )],
                )),
                list.id,
            ))
            .await
            .expect("Tool list response");

        let ClientJsonRpcMessage::Request(call) =
            server.receive().await.expect("Tool call request")
        else {
            panic!("expected Tool call request");
        };
        server
            .send(ServerJsonRpcMessage::request(
                ServerRequest::ListRootsRequest(ListRootsRequest::default()),
                RequestId::Number(99),
            ))
            .await
            .expect("Roots request");
        let ClientJsonRpcMessage::Response(response) =
            server.receive().await.expect("Roots response")
        else {
            panic!("expected Roots response");
        };
        let ClientResult::ListRootsResult(roots) = response.result else {
            panic!("expected Roots result");
        };
        assert_eq!(roots.roots[0].uri, "file:///workspace");

        server
            .send(ServerJsonRpcMessage::request(
                ServerRequest::CreateMessageRequest(CreateMessageRequest::new(
                    CreateMessageRequestParams::new(
                        vec![SamplingMessage::user_text("sample this")],
                        64,
                    )
                    .with_system_prompt("system")
                    .with_temperature(0.25),
                )),
                RequestId::Number(100),
            ))
            .await
            .expect("Sampling request");
        let ClientJsonRpcMessage::Response(response) =
            server.receive().await.expect("Sampling response")
        else {
            panic!("expected Sampling response");
        };
        let ClientResult::CreateMessageResult(result) = response.result else {
            panic!("expected Sampling result");
        };
        assert_eq!(result.model, "host-model");
        assert_eq!(result.message, SamplingMessage::assistant_text("sampled"));

        server
            .send(ServerJsonRpcMessage::request(
                ServerRequest::ElicitRequest(ElicitRequest::new(
                    ElicitRequestParams::UrlElicitationParams {
                        meta: None,
                        message: "Approve access".to_string(),
                        url: "https://example.test/approve".to_string(),
                        elicitation_id: "elicit-1".to_string(),
                    },
                )),
                RequestId::Number(101),
            ))
            .await
            .expect("Elicitation request");
        let ClientJsonRpcMessage::Response(response) =
            server.receive().await.expect("Elicitation response")
        else {
            panic!("expected Elicitation response");
        };
        let ClientResult::ElicitResult(result) = response.result else {
            panic!("expected Elicitation result");
        };
        assert_eq!(result.action, ElicitationAction::Accept);
        assert_eq!(
            result.content,
            Some(serde_json::json!({ "approved": true }))
        );
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::CallToolResult(CallToolResult::success(vec![
                    rmcp::model::ContentBlock::text("ok"),
                ])),
                call.id,
            ))
            .await
            .expect("Tool result");
    });
    let host = Arc::new(CapturingHost(Mutex::new(Vec::new())));
    let server_id = McpServerId::try_from("host-server").expect("Server id");
    let client = LegacyClient::connect_with_host(
        server_id.clone(),
        client_transport,
        Arc::clone(&host) as Arc<dyn McpHost>,
    )
    .await
    .expect("Legacy client");
    client.discover().await.expect("Tool catalog");
    let context = McpRequestContext::builder()
        .server_id(server_id.clone())
        .session_id(SessionId::try_from("session-host").expect("Session id"))
        .turn_id(TurnId::try_from("turn-host").expect("Turn id"))
        .trace_id(TraceId::try_from("trace-host").expect("Trace id"))
        .requested_at_ms(TimestampMs::from(123))
        .build();
    client
        .call_tool(
            McpToolRequest {
                reference: McpToolRef {
                    server_id,
                    remote_name: "host_tool".to_string(),
                },
                arguments: serde_json::Map::new(),
            },
            McpRequestControl::builder()
                .context(context.clone())
                .cancellation(CancellationToken::new())
                .lifecycle(CancellationToken::new())
                .idle_timeout(Duration::from_secs(1))
                .total_timeout(Duration::from_secs(2))
                .max_rounds(NonZeroU32::new(2).expect("non-zero rounds"))
                .progress(Arc::new(NoProgress))
                .build(),
        )
        .await
        .expect("Tool call");

    {
        let requests = host.0.lock().expect("Host request lock");
        assert_eq!(requests.len(), 3);
        let correlated = |actual: &McpRequestContext| {
            actual.server_id == context.server_id
                && actual.session_id == context.session_id
                && actual.turn_id == context.turn_id
                && actual.trace_id == context.trace_id
                && actual.requested_at_ms >= context.requested_at_ms
        };
        assert!(requests.iter().all(|request| match request {
            McpHostRequest::Roots(request) => correlated(&request.context),
            McpHostRequest::Sampling(request) => {
                correlated(&request.context)
                    && request.max_tokens == 64
                    && request.temperature == Some(0.25)
                    && request.system_prompt.as_deref() == Some("system")
            }
            McpHostRequest::Elicitation(request) => {
                correlated(&request.context)
                    && matches!(
                        &request.mode,
                        McpElicitationMode::Url { elicitation_id, .. }
                            if elicitation_id == "elicit-1"
                    )
            }
            McpHostRequest::Authorization(_) => false,
        }));
    }
    client.shutdown().await.expect("Legacy shutdown");
    server_task.await.expect("reference Server");
}
