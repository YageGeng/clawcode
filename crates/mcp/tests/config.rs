use std::collections::HashMap;
use std::time::Duration;

use config::McpServerConfig;
use mcp::{McpAuthentication, McpTransport, RuntimeMcpServer};
use protocol::McpProtocolVersion;

/// Stdio TOML fields become one validated runtime transport.
#[test]
fn converts_stdio_server_config() {
    let config = McpServerConfig::builder()
        .name("local".to_string())
        .protocol("2025-11-25".to_string())
        .command("node".to_string())
        .args(vec!["server.js".to_string()])
        .env(HashMap::from([("TOKEN".to_string(), "value".to_string())]))
        .build();

    let runtime =
        RuntimeMcpServer::try_from(config).expect("valid stdio config");

    assert!(matches!(
        runtime.transport,
        McpTransport::Stdio(config)
            if config.command == "node"
                && config.args == vec!["server.js"]
                && config.env.get("TOKEN") == Some(&"value".to_string())
    ));
    assert_eq!(runtime.protocol, McpProtocolVersion::V2025_11_25);
    assert_eq!(runtime.startup_timeout, Duration::from_secs(30));
    assert_eq!(runtime.request_timeout, Duration::from_secs(120));
    assert_eq!(runtime.mrtr.max_rounds.get(), 8);
    assert_eq!(runtime.mrtr.total_timeout, Duration::from_secs(300));
}

/// Streamable HTTP TOML fields preserve URL and custom headers.
#[test]
fn converts_streamable_http_server_config() {
    let config = McpServerConfig::builder()
        .name("remote".to_string())
        .protocol("2026-07-28".to_string())
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
        McpTransport::StreamableHttp(config)
            if config.url.as_str() == "https://example.com/mcp"
                && config.headers.get("X-Tenant").and_then(|value| value.to_str().ok())
                    == Some("one")
                && config.authentication == McpAuthentication::None
    ));
    assert_eq!(runtime.protocol, McpProtocolVersion::V2026_07_28);
}

/// Runtime conversion rejects malformed URLs and HTTP headers before connecting.
#[test]
fn rejects_invalid_http_endpoint_and_headers() {
    let invalid_url = McpServerConfig::builder()
        .name("invalid-url".to_string())
        .protocol("2026-07-28".to_string())
        .url("not a URL".to_string())
        .build();
    RuntimeMcpServer::try_from(invalid_url).expect_err("invalid URL");

    let invalid_header = McpServerConfig::builder()
        .name("invalid-header".to_string())
        .protocol("2026-07-28".to_string())
        .url("https://example.com/mcp".to_string())
        .http_headers(HashMap::from([(
            "bad header".to_string(),
            "value".to_string(),
        )]))
        .build();
    RuntimeMcpServer::try_from(invalid_header).expect_err("invalid header");
}
