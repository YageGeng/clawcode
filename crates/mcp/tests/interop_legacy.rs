mod interop_support;

use std::collections::HashMap;
use std::sync::Arc;

use interop_support::{
    InteropConfig, RejectingHost, reference_server::ReferenceServer,
    verify_client,
};
use mcp::{McpConnector, RmcpConnector};
use protocol::{McpProtocolVersion, SessionId};
use rmcp::ServiceExt;

const SERVER_PROCESS_ENV: &str = "CLAWCODE_MCP_LEGACY_REFERENCE";

/// Runs the reusable reference Server over this test process's stdin and stdout.
#[tokio::test]
async fn legacy_stdio_reference_process()
-> Result<(), Box<dyn std::error::Error>> {
    if std::env::var(SERVER_PROCESS_ENV).as_deref() != Ok("1") {
        return Ok(());
    }
    let server = ReferenceServer::legacy()
        .serve(rmcp::transport::stdio())
        .await?;
    server.waiting().await?;
    Ok(())
}

/// Verifies exact Legacy lifecycle and baseline capabilities over real stdio pipes.
#[tokio::test]
#[ignore = "runs a real MCP stdio child process"]
async fn interoperates_with_legacy_stdio_server()
-> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let config = InteropConfig::legacy(
        executable.to_string_lossy().into_owned(),
        vec![
            "--exact".to_string(),
            "legacy_stdio_reference_process".to_string(),
            "--quiet".to_string(),
            "--nocapture".to_string(),
            "--test-threads".to_string(),
            "1".to_string(),
        ],
        HashMap::from([(SERVER_PROCESS_ENV.to_string(), "1".to_string())]),
    );
    let session_id = SessionId::try_from("legacy-interop")?;
    let client = RmcpConnector::default()
        .connect(&config, &session_id, Arc::new(RejectingHost))
        .await?;

    verify_client(
        client.as_ref(),
        &config.server_id,
        McpProtocolVersion::V2025_11_25,
    )
    .await?;
    client.shutdown().await?;
    Ok(())
}
