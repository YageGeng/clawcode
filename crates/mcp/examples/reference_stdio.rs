mod reference_server;

use reference_server::ReferenceServer;
use rmcp::ServiceExt;

/// Runs the initialize reference Server over stdin and stdout.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let version = std::env::args()
        .nth(1)
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(protocol::McpProtocolVersion::V2025_11_25);
    let server = ReferenceServer::new(version)
        .serve(rmcp::transport::stdio())
        .await?;
    server.waiting().await?;
    Ok(())
}
