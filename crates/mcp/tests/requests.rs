use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use mcp::{
    InitializeClient, McpClient, McpError, McpProgress, McpProgressSink,
    McpRequestControl,
};
use protocol::{
    McpRequestContext, McpServerId, McpToolRef, McpToolRequest, SessionId,
    TimestampMs, TraceId, TurnId,
};
use rmcp::model::{
    CallToolResult, ClientJsonRpcMessage, InitializeResult, ProtocolVersion,
    ServerCapabilities, ServerJsonRpcMessage, ServerResult,
};
use rmcp::transport::{IntoTransport, Transport};
use tokio_util::sync::CancellationToken;

/// Observes forwarded progress without altering request deadlines.
struct ProgressCount(Arc<AtomicUsize>);
impl McpProgressSink for ProgressCount {
    /// Counts progress received through the real rmcp notification handler.
    fn publish(&self, _progress: McpProgress) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// Resource cancellation reaches the server as a correlated protocol notification.
#[tokio::test]
async fn resource_cancellation_reaches_transport() {
    let (server_stream, client_stream) = tokio::io::duplex(8192);
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let peer = tokio::spawn(async move {
        let mut server =
            IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
                server_stream,
            );
        let ClientJsonRpcMessage::Request(initialize) =
            server.receive().await.expect("initialize")
        else {
            panic!("request")
        };
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::InitializeResult(
                    InitializeResult::new(
                        ServerCapabilities::builder()
                            .enable_resources()
                            .build(),
                    )
                    .with_protocol_version(ProtocolVersion::V_2025_11_25),
                ),
                initialize.id,
            ))
            .await
            .expect("response");
        let _ = server.receive().await.expect("initialized");
        let ClientJsonRpcMessage::Request(read) =
            server.receive().await.expect("read")
        else {
            panic!("request")
        };
        assert!(matches!(
            read.request,
            rmcp::model::ClientRequest::ReadResourceRequest(_)
        ));
        cancel.cancel();
        let notice =
            serde_json::to_value(server.receive().await.expect("cancellation"))
                .expect("JSON");
        assert_eq!(notice["method"], "notifications/cancelled");
        assert_eq!(
            notice["params"]["requestId"],
            serde_json::to_value(read.id).expect("id")
        );
    });
    let id = McpServerId::try_from("resources").expect("id");
    let client = InitializeClient::connect(id.clone(), client_stream)
        .await
        .expect("client");
    let result = client
        .read_resource(
            protocol::McpResourceRequest {
                reference: protocol::McpResourceRef {
                    server_id: id,
                    remote_uri: "opaque://not-listed".into(),
                },
            },
            &cancellation,
        )
        .await;
    assert!(matches!(result, Err(McpError::RequestCancelled(_))));
    peer.await.expect("server");
    client.shutdown().await.expect("shutdown");
}

/// Progress extends inactivity deadlines but cannot keep one request alive indefinitely.
#[tokio::test(start_paused = true)]
async fn progress_resets_idle_but_not_total_deadline() {
    for complete in [true, false] {
        let (server_stream, client_stream) = tokio::io::duplex(8192);
        let mut server =
            IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
                server_stream,
            );
        let peer = tokio::spawn(async move {
            let ClientJsonRpcMessage::Request(initialize) =
                server.receive().await.expect("initialize")
            else {
                panic!("request")
            };
            server
                .send(ServerJsonRpcMessage::response(
                    ServerResult::InitializeResult(
                        InitializeResult::new(
                            ServerCapabilities::builder()
                                .enable_tools()
                                .build(),
                        )
                        .with_protocol_version(ProtocolVersion::V_2025_11_25),
                    ),
                    initialize.id,
                ))
                .await
                .expect("initialize response");
            assert!(matches!(
                server.receive().await,
                Some(ClientJsonRpcMessage::Notification(_))
            ));
            let ClientJsonRpcMessage::Request(call) =
                server.receive().await.expect("call")
            else {
                panic!("request")
            };
            let value =
                serde_json::to_value(&call.request).expect("request JSON");
            let token = value["params"]["_meta"]["progressToken"].clone();
            assert!(!token.is_null());
            for progress in 1..=8 {
                tokio::select! {
                    message = server.receive() => {
                        let value = serde_json::to_value(message.expect("cancel notification")).expect("notification JSON");
                        assert_eq!(value["method"], "notifications/cancelled");
                        return true;
                    }
                    () = tokio::time::sleep(Duration::from_millis(25)) => {}
                }
                server
                    .send(
                        serde_json::from_value(serde_json::json!({
                            "jsonrpc":"2.0", "method":"notifications/progress",
                            "params":{"progressToken":token,"progress":progress}
                        }))
                        .expect("progress notification"),
                    )
                    .await
                    .expect("progress send");
                if complete && progress == 3 {
                    server
                        .send(ServerJsonRpcMessage::response(
                            ServerResult::CallToolResult(
                                CallToolResult::success(vec![]),
                            ),
                            call.id,
                        ))
                        .await
                        .expect("result");
                    return false;
                }
            }
            panic!("total deadline must cancel the request");
        });
        let id = McpServerId::try_from("requests").expect("server id");
        let client = InitializeClient::connect(id.clone(), client_stream)
            .await
            .expect("connect");
        let progress = Arc::new(AtomicUsize::new(0));
        let result = client
            .call_tool(
                McpToolRequest {
                    reference: McpToolRef {
                        server_id: id.clone(),
                        remote_name: "slow".to_string(),
                    },
                    arguments: Default::default(),
                },
                McpRequestControl::builder()
                    .context(
                        McpRequestContext::builder()
                            .server_id(id)
                            .session_id(
                                SessionId::try_from("session").expect("id"),
                            )
                            .turn_id(TurnId::try_from("turn").expect("id"))
                            .trace_id(TraceId::try_from("trace").expect("id"))
                            .requested_at_ms(TimestampMs::now())
                            .build(),
                    )
                    .cancellation(CancellationToken::new())
                    .lifecycle(CancellationToken::new())
                    .idle_timeout(Duration::from_millis(40))
                    .total_timeout(Duration::from_millis(110))
                    .progress(Arc::new(ProgressCount(Arc::clone(&progress))))
                    .build(),
            )
            .await;
        assert_eq!(peer.await.expect("server task"), !complete);
        client.shutdown().await.expect("shutdown");
        assert!(progress.load(Ordering::Relaxed) >= 2);
        if complete {
            result.expect("progress prevents idle timeout");
        } else {
            assert!(
                matches!(&result, Err(McpError::RequestTimeout(_))),
                "total deadline result: {result:?}"
            );
        }
    }
}

/// Simulates modest synchronous progress projection cost while the queue stays ready.
struct SlowProgress(Arc<std::sync::atomic::AtomicUsize>);
impl McpProgressSink for SlowProgress {
    /// Charges two milliseconds per progress projection.
    fn publish(&self, _progress: McpProgress) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(2));
    }
}
/// A busy progress channel must not outrank an already expired absolute deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn continuous_progress_cannot_starve_total_deadline() {
    let (server_stream, client_stream) = tokio::io::duplex(65536);
    let peer = tokio::spawn(async move {
        let mut server =
            IntoTransport::<rmcp::RoleServer, _, _>::into_transport(
                server_stream,
            );
        let ClientJsonRpcMessage::Request(init) =
            server.receive().await.expect("init")
        else {
            panic!("request")
        };
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::InitializeResult(
                    InitializeResult::new(
                        ServerCapabilities::builder().enable_tools().build(),
                    )
                    .with_protocol_version(ProtocolVersion::V_2025_11_25),
                ),
                init.id,
            ))
            .await
            .expect("reply");
        let _ = server.receive().await;
        let ClientJsonRpcMessage::Request(call) =
            server.receive().await.expect("call")
        else {
            panic!("request")
        };
        let token = serde_json::to_value(call.request).expect("json")["params"]
            ["_meta"]["progressToken"]
            .clone();
        tokio::time::sleep(Duration::from_millis(5)).await;
        for progress in 0..2000 {
            let notice = serde_json::from_value(serde_json::json!({
                "jsonrpc": "2.0", "method": "notifications/progress",
                "params": { "progressToken": token, "progress": progress }
            }))
            .expect("notice");
            if server.send(notice).await.is_err() {
                break;
            }
        }
        std::future::pending::<()>().await;
    });
    let id = McpServerId::try_from("flood").expect("id");
    let client = InitializeClient::connect(id.clone(), client_stream)
        .await
        .expect("client");
    let context = McpRequestContext::builder()
        .server_id(id.clone())
        .session_id(SessionId::try_from("session").expect("id"))
        .turn_id(TurnId::try_from("turn").expect("id"))
        .trace_id(TraceId::try_from("trace").expect("id"))
        .requested_at_ms(TimestampMs::now())
        .build();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let control = McpRequestControl::builder()
        .context(context)
        .cancellation(CancellationToken::new())
        .lifecycle(CancellationToken::new())
        .idle_timeout(Duration::from_secs(5))
        .total_timeout(Duration::from_millis(20))
        .progress(Arc::new(SlowProgress(Arc::clone(&count))))
        .build();

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        client.call_tool(
            McpToolRequest {
                reference: McpToolRef {
                    server_id: id,
                    remote_name: "busy".into(),
                },
                arguments: Default::default(),
            },
            control,
        ),
    )
    .await;

    client.shutdown().await.expect("shutdown");
    peer.abort();
    let _ = peer.await;
    assert!(
        matches!(result, Ok(Err(McpError::RequestTimeout(_)))),
        "absolute deadline must settle the request"
    );
    assert!(
        count.load(Ordering::Relaxed) <= 20,
        "ready progress must not outrank the deadline"
    );
}
