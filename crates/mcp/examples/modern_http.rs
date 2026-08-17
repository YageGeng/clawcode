mod reference_server;

use std::net::SocketAddr;

use reference_server::ReferenceServer;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
};

/// Runs the exact Modern reference Server over Streamable HTTP.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:39128".to_string())
        .parse::<SocketAddr>()?;
    let service: StreamableHttpService<ReferenceServer, LocalSessionManager> =
        StreamableHttpService::new(
            || Ok(ReferenceServer::modern()),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(false)
                .with_json_response(true),
        );
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let address = listener.local_addr()?;
    eprintln!("Modern MCP reference Server listening at http://{address}/mcp");
    axum::serve(listener, axum::Router::new().nest_service("/mcp", service))
        .await?;
    Ok(())
}
