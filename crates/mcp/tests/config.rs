use std::collections::HashMap;

use config::McpServerConfig;
use mcp::{McpTransport, RuntimeMcpServer};

/// Stdio TOML fields become one validated runtime transport.
#[test]
fn converts_stdio_server_config() {
    let config = McpServerConfig::builder()
        .name("local".to_string())
        .command("node".to_string())
        .args(vec!["server.js".to_string()])
        .env(HashMap::from([("TOKEN".to_string(), "value".to_string())]))
        .build();

    let runtime =
        RuntimeMcpServer::try_from(config).expect("valid stdio config");

    assert!(matches!(
        runtime.transport,
        McpTransport::Stdio { command, args, env }
            if command == "node"
                && args == vec!["server.js"]
                && env.get("TOKEN") == Some(&"value".to_string())
    ));
}

/// Streamable HTTP TOML fields preserve URL and custom headers.
#[test]
fn converts_streamable_http_server_config() {
    let config = McpServerConfig::builder()
        .name("remote".to_string())
        .url("https://example.com/mcp".to_string())
        .http_headers(HashMap::from([(
            "X-Tenant".to_string(),
            "one".to_string(),
        )]))
        .build();

    let runtime =
        RuntimeMcpServer::try_from(config).expect("valid HTTP config");

    assert!(matches!(
        runtime.transport,
        McpTransport::StreamableHttp { url, headers, .. }
            if url == "https://example.com/mcp"
                && headers.get("X-Tenant") == Some(&"one".to_string())
    ));
}
