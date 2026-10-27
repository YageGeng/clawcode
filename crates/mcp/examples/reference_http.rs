mod reference_server;

use std::net::SocketAddr;

use reference_server::ReferenceServer;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
};

/// Runs the exact Initialize reference Server over Streamable HTTP.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:39128".to_string())
        .parse::<SocketAddr>()?;
    let version = std::env::args()
        .nth(2)
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(protocol::McpProtocolVersion::V2025_11_25);
    let service: StreamableHttpService<ReferenceServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(ReferenceServer::new(version)),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(true)
                .with_json_response(true),
        );
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let address = listener.local_addr()?;
    eprintln!(
        "Initialize MCP reference Server listening at http://{address}/mcp"
    );
    axum::serve(listener, axum::Router::new().nest_service("/mcp", service))
        .await?;
    Ok(())
}
