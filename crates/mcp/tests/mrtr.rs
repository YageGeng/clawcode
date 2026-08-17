#![expect(
    deprecated,
    reason = "MRTR Roots input remains part of the supported MCP 2026-07-28 protocol"
)]
#![expect(
    clippy::panic,
    reason = "protocol fixtures require exhaustive assertions"
)]

mod support;

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mcp::{
    McpClient, McpError, McpHost, McpProgress, McpProgressSink,
    McpRequestControl, ModernClient,
};
use protocol::{
    McpHostRequest, McpHostResponse, McpRequestContext, McpRoot,
    McpRootsResult, McpToolRef, McpToolRequest, SessionId, TimestampMs,
    TraceId, TurnId,
};
use rmcp::model::{
    CallToolResult, ClientJsonRpcMessage, ClientRequest, DiscoverResult,
    Implementation, InputRequest, InputRequests, InputRequiredResult,
    ListRootsRequest, ProtocolVersion, ServerJsonRpcMessage, ServerResult,
};
use rmcp::transport::{IntoTransport, Transport};
use tokio_util::sync::CancellationToken;

/// Host fixture that authorizes one root requested through an MRTR input round.
struct RootsHost;

#[async_trait]
impl McpHost for RootsHost {
    /// Handles only the Roots callback exercised by this protocol test.
    async fn handle(
        &self,
        request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        let McpHostRequest::Roots(_request) = request else {
            panic!("expected Roots request");
        };
        Ok(McpHostResponse::Roots(McpRootsResult {
            roots: vec![McpRoot {
                uri: "file:///workspace".to_string(),
                name: Some("workspace".to_string()),
            }],
        }))
    }
}

/// Progress sink intentionally ignores updates in the MRTR protocol test.
struct NoProgress;

impl McpProgressSink for NoProgress {
    /// Accepts one progress update without introducing unrelated assertions.
    fn publish(&self, _progress: McpProgress) {}
}

/// Builds the correlated request control shared by all MRTR rounds.
fn request_control() -> McpRequestControl {
    McpRequestControl::builder()
        .context(
            McpRequestContext::builder()
                .server_id(support::server_id())
                .session_id(
                    SessionId::try_from("session-mrtr").expect("Session id"),
                )
                .turn_id(TurnId::try_from("turn-mrtr").expect("Turn id"))
                .trace_id(TraceId::try_from("trace-mrtr").expect("Trace id"))
                .requested_at_ms(TimestampMs::now())
                .build(),
        )
        .cancellation(CancellationToken::new())
        .lifecycle(CancellationToken::new())
        .idle_timeout(Duration::from_secs(1))
        .total_timeout(Duration::from_secs(2))
        .max_rounds(NonZeroU32::new(2).expect("non-zero rounds"))
        .progress(Arc::new(NoProgress))
        .build()
}

/// Modern Tool calls fulfill input requests and echo opaque state before completing.
#[tokio::test]
async fn modern_tool_drives_mrtr_to_completion() {
    let (server_transport, client_transport) = tokio::io::duplex(16_384);
    let mut server = IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
        server_transport,
    );
    let server_task = tokio::spawn(async move {
        let ClientJsonRpcMessage::Request(discover) =
            server.receive().await.expect("discover request")
        else {
            panic!("expected discover request");
        };
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::DiscoverResult(
                    DiscoverResult::new(
                        vec![ProtocolVersion::V_2026_07_28],
                        support::tools_capabilities(),
                    )
                    .with_server_info(Implementation::new(
                        "mrtr-server",
                        "1.0.0",
                    )),
                ),
                discover.id,
            ))
            .await
            .expect("discover response");

        let ClientJsonRpcMessage::Request(first) =
            server.receive().await.expect("first Tool call")
        else {
            panic!("expected first Tool call");
        };
        let ClientRequest::CallToolRequest(first_call) = first.request else {
            panic!("expected tools/call");
        };
        assert!(first_call.params.input_responses.is_none());
        assert!(first_call.params.request_state.is_none());
        let mut requests = InputRequests::new();
        requests.insert(
            "roots".to_string(),
            InputRequest::ListRoots(ListRootsRequest::default()),
        );
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::InputRequiredResult(InputRequiredResult::new(
                    Some(requests),
                    Some("opaque state +/=".to_string()),
                )),
                first.id,
            ))
            .await
            .expect("input-required response");

        let ClientJsonRpcMessage::Request(second) =
            server.receive().await.expect("second Tool call")
        else {
            panic!("expected second Tool call");
        };
        let ClientRequest::CallToolRequest(second_call) = second.request else {
            panic!("expected tools/call retry");
        };
        assert_eq!(
            second_call.params.request_state.as_deref(),
            Some("opaque state +/=")
        );
        let roots = second_call
            .params
            .input_responses
            .as_ref()
            .and_then(|responses| responses.get("roots"))
            .expect("Roots response");
        assert_eq!(roots["roots"][0]["uri"], "file:///workspace");
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::CallToolResult(CallToolResult::success(vec![
                    rmcp::model::ContentBlock::text("done"),
                ])),
                second.id,
            ))
            .await
            .expect("Tool completion");
    });

    let client = ModernClient::connect_with_host(
        support::server_id(),
        client_transport,
        Arc::new(RootsHost),
    )
    .await
    .expect("Modern client");
    let result = client
        .call_tool(
            McpToolRequest {
                reference: McpToolRef {
                    server_id: support::server_id(),
                    remote_name: "mrtr_tool".to_string(),
                },
                arguments: serde_json::Map::new(),
            },
            request_control(),
        )
        .await
        .expect("MRTR Tool result");
    assert_eq!(result.content.len(), 1);
    client.shutdown().await.expect("Modern shutdown");
    server_task.await.expect("reference Server");
}
