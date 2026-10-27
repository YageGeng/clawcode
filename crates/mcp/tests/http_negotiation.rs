use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    Json,
    body::{Body, Bytes},
    extract::State,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use mcp::{
    McpConnector, McpError, McpHost, McpProgress, McpProgressSink,
    McpRequestControl, RmcpConnector, RuntimeMcpServer,
};
use protocol::{
    McpHostRequest, McpHostResponse, McpProtocolPolicy, McpProtocolVersion,
    McpRequestContext, McpServerId, McpToolRef, McpToolRequest, SessionId,
    TimestampMs, TraceId, TurnId,
};
use tokio_util::sync::CancellationToken;

/// Retains only protocol metadata, never authentication headers or request bodies.
struct ObservedRequest {
    method: String,
    header: Option<String>,
    requested: Option<String>,
}

/// Minimal HTTP peer that expires its session on the first tool call.
#[derive(Clone, Default)]
struct HttpProbe(Arc<Mutex<Vec<ObservedRequest>>>);

impl HttpProbe {
    /// Records negotiated headers and returns a single valid initialize response.
    async fn respond(
        State(probe): State<Self>,
        request: axum::http::Request<Body>,
    ) -> Response {
        let method = request.method().clone();
        let header = request
            .headers()
            .get("MCP-Protocol-Version")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = axum::body::to_bytes(request.into_body(), 8192)
            .await
            .unwrap_or_else(|_| Bytes::new());
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or_default();
        let rpc = body
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(method.as_str())
            .to_string();
        probe.0.lock().expect("requests").push(ObservedRequest {
            method: rpc.clone(),
            header,
            requested: body
                .pointer("/params/protocolVersion")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        });
        if method == Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        if method == Method::DELETE {
            return StatusCode::NO_CONTENT.into_response();
        }
        match rpc.as_str() {
            "initialize" => ([("Mcp-Session-Id", "probe-session")], Json(serde_json::json!({
                "jsonrpc":"2.0", "id":body.get("id"), "result":{
                    "protocolVersion":"2025-06-18", "capabilities":{"tools":{}},
                    "serverInfo":{"name":"probe","version":"1"}
                }
            }))).into_response(),
            "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }
}

/// This server should not issue application callbacks or progress.
struct NoCallbacks;
#[async_trait]
impl McpHost for NoCallbacks {
    /// Rejects unexpected server callbacks in the transport-only scenario.
    async fn handle(
        &self,
        _request: McpHostRequest,
    ) -> Result<McpHostResponse, McpError> {
        Err(McpError::Host("unexpected callback".to_string()))
    }
}
impl McpProgressSink for NoCallbacks {
    /// No progress is expected from the immediate HTTP 404 response.
    fn publish(&self, _progress: McpProgress) {}
}

/// Auto and preferred requests both use the selected header and never bypass the guard on HTTP 404.
#[tokio::test]
async fn http_negotiation_preserves_headers_and_rejects_expired_sessions() {
    for (preference, requested) in
        [("auto", "2025-11-25"), ("2024-11-05", "2024-11-05")]
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let url =
            format!("http://{}/mcp", listener.local_addr().expect("address"));
        let probe = HttpProbe::default();
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let router = axum::Router::new()
            .route("/mcp", any(HttpProbe::respond))
            .with_state(probe.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .expect("serve");
        });
        let raw = config::McpServerConfig::builder()
            .name("probe".to_string())
            .protocol(preference.to_string())
            .url(url)
            .build();
        let config = RuntimeMcpServer::try_from(raw).expect("runtime config");
        assert_eq!(
            config.protocol_policy,
            preference.parse::<McpProtocolPolicy>().expect("policy")
        );
        let session = SessionId::try_from("http-probe").expect("session");
        let client = RmcpConnector::default()
            .connect(&config, &session, Arc::new(NoCallbacks))
            .await
            .expect("connect");
        assert_eq!(client.protocol(), McpProtocolVersion::V2025_06_18);
        let id = McpServerId::try_from("probe").expect("server");
        let result = client
            .call_tool(
                McpToolRequest {
                    reference: McpToolRef {
                        server_id: id.clone(),
                        remote_name: "test".to_string(),
                    },
                    arguments: Default::default(),
                },
                McpRequestControl::builder()
                    .context(
                        McpRequestContext::builder()
                            .server_id(id)
                            .session_id(session)
                            .turn_id(TurnId::try_from("turn").expect("turn"))
                            .trace_id(
                                TraceId::try_from("trace").expect("trace"),
                            )
                            .requested_at_ms(TimestampMs::now())
                            .build(),
                    )
                    .cancellation(CancellationToken::new())
                    .lifecycle(CancellationToken::new())
                    .idle_timeout(Duration::from_secs(2))
                    .total_timeout(Duration::from_secs(2))
                    .progress(Arc::new(NoCallbacks))
                    .build(),
            )
            .await;
        client.shutdown().await.expect("close client");
        shutdown.cancel();
        server.await.expect("server task");
        assert!(
            result.is_err(),
            "the expired session must not be transparently reused"
        );
        let requests = probe.0.lock().expect("requests");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "initialize")
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .find(|request| request.method == "initialize")
                .and_then(|request| request.requested.as_deref()),
            Some(requested)
        );
        for request in requests
            .iter()
            .filter(|request| request.method != "initialize")
        {
            assert_eq!(
                request.header.as_deref(),
                Some("2025-06-18"),
                "{} header",
                request.method
            );
        }
    }
}
