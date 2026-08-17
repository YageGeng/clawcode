mod interop_support;

use std::sync::Arc;

use interop_support::{
    InteropConfig, RejectingHost, reference_server::ReferenceServer,
    verify_client,
};
use mcp::{McpConnector, RmcpConnector};
use protocol::{McpProtocolVersion, SessionId};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

/// Verifies exact Modern discovery and baseline capabilities over real Streamable HTTP.
#[tokio::test]
#[ignore = "runs a real MCP Streamable HTTP server"]
async fn interoperates_with_modern_http_server()
-> Result<(), Box<dyn std::error::Error>> {
    let shutdown = CancellationToken::new();
    let service: StreamableHttpService<ReferenceServer, LocalSessionManager> =
        StreamableHttpService::new(
            || Ok(ReferenceServer::modern()),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(false)
                .with_json_response(true)
                .with_cancellation_token(shutdown.child_token()),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_task = tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            axum::serve(
                listener,
                axum::Router::new().nest_service("/mcp", service),
            )
            .with_graceful_shutdown(async move {
                shutdown.cancelled_owned().await;
            })
            .await
        }
    });
    let config = InteropConfig::modern(format!("http://{address}/mcp"));
    let session_id = SessionId::try_from("modern-interop")?;
    let client = RmcpConnector::default()
        .connect(&config, &session_id, Arc::new(RejectingHost))
        .await?;

    verify_client(
        client.as_ref(),
        &config.server_id,
        McpProtocolVersion::V2026_07_28,
    )
    .await?;
    client.shutdown().await?;
    shutdown.cancel();
    server_task.await??;
    Ok(())
}
