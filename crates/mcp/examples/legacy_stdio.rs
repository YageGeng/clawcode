mod reference_server;

use reference_server::ReferenceServer;
use rmcp::ServiceExt;

/// Runs the exact Legacy reference Server over stdin and stdout.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = ReferenceServer::legacy()
        .serve(rmcp::transport::stdio())
        .await?;
    server.waiting().await?;
    Ok(())
}
