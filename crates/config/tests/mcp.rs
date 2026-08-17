use config::{
    AppConfig, ConfigValidationError, McpConfigError, McpServerConfig,
};

/// Builds a complete application document around MCP snippets so cross-field validation runs.
fn application_document(mcp_servers: &str) -> String {
    format!(
        r#"
active_model = "test/model"

[[providers]]
id = "test"
display_name = "Test"
base_url = "https://example.com"
api_key = "test-key"

[[providers.models]]
id = "model"
context_tokens = 100000
max_output_tokens = 10000

{mcp_servers}
"#
    )
}

/// Every MCP Server must select one exact supported protocol revision.
#[test]
fn protocol_is_required_and_exact() {
    let missing: McpServerConfig = toml::from_str(
        r#"
name = "missing"
command = "server"
"#,
    )
    .expect("parse config with missing protocol");
    assert!(matches!(
        missing.validate(),
        Err(McpConfigError::MissingProtocol { server }) if server == "missing"
    ));

    let unknown: McpServerConfig = toml::from_str(
        r#"
name = "unknown"
protocol = "2026-01-01"
command = "server"
"#,
    )
    .expect("parse config with unknown protocol");
    assert!(matches!(
        unknown.validate(),
        Err(McpConfigError::UnsupportedProtocol { server, protocol })
            if server == "unknown" && protocol == "2026-01-01"
    ));
}

/// Application validation rejects duplicate and namespace-unsafe Server names.
#[test]
fn server_names_are_unique_and_namespace_safe() {
    let duplicate: AppConfig = toml::from_str(&application_document(
        r#"
[[mcp_servers]]
name = "same"
protocol = "2025-11-25"
command = "legacy"

[[mcp_servers]]
name = "same"
protocol = "2026-07-28"
url = "https://example.com/mcp"
"#,
    ))
    .expect("parse duplicate MCP names");
    assert!(matches!(
        duplicate.validate(),
        Err(ConfigValidationError::Mcp(McpConfigError::DuplicateName {
            server
        })) if server == "same"
    ));

    let invalid: McpServerConfig = toml::from_str(
        r#"
name = "not safe"
protocol = "2025-11-25"
command = "server"
"#,
    )
    .expect("parse namespace-unsafe name");
    assert!(matches!(
        invalid.validate(),
        Err(McpConfigError::InvalidName { server, .. }) if server == "not safe"
    ));
}

/// Transport fields must select exactly one non-empty transport endpoint.
#[test]
fn transport_is_exclusive_and_non_empty() {
    for (document, expected_reason) in [
        (
            r#"
name = "both"
protocol = "2025-11-25"
command = "server"
url = "https://example.com/mcp"
"#,
            "configure either command or url, not both",
        ),
        (
            r#"
name = "empty-command"
protocol = "2025-11-25"
command = "  "
"#,
            "command must not be empty",
        ),
        (
            r#"
name = "empty-url"
protocol = "2026-07-28"
url = ""
"#,
            "url must not be empty",
        ),
    ] {
        let config: McpServerConfig =
            toml::from_str(document).expect("parse invalid transport");
        assert!(matches!(
            config.validate(),
            Err(McpConfigError::InvalidTransport { reason, .. })
                if reason == expected_reason
        ));
    }
}

/// Startup, request, and MRTR limits reject zero before runtime construction.
#[test]
fn timeouts_and_mrtr_limits_must_be_positive() {
    for (field, value) in [
        ("startup_timeout_sec", 0_u64),
        ("request_timeout_sec", 0_u64),
        ("mrtr_total_timeout_sec", 0_u64),
        ("mrtr_max_rounds", 0_u64),
    ] {
        let document = format!(
            r#"
name = "limits"
protocol = "2026-07-28"
url = "https://example.com/mcp"
{field} = {value}
"#
        );
        let config: McpServerConfig =
            toml::from_str(&document).expect("parse zero limit");
        assert!(matches!(
            config.validate(),
            Err(McpConfigError::InvalidLimit {
                field: invalid_field,
                ..
            }) if invalid_field == field
        ));
    }
}

/// OAuth is HTTP-only and cannot be combined with static bearer authentication.
#[test]
fn authentication_modes_are_transport_safe_and_exclusive() {
    let stdio_oauth: McpServerConfig = toml::from_str(
        r#"
name = "stdio-oauth"
protocol = "2025-11-25"
command = "server"

[oauth]
client_id = "client"
"#,
    )
    .expect("parse stdio OAuth");
    assert!(matches!(
        stdio_oauth.validate(),
        Err(McpConfigError::InvalidAuthentication { .. })
    ));

    let conflicting: McpServerConfig = toml::from_str(
        r#"
name = "conflicting-auth"
protocol = "2026-07-28"
url = "https://example.com/mcp"
bearer_token_env = "MCP_TOKEN"

[oauth]
client_id = "client"
"#,
    )
    .expect("parse conflicting HTTP auth");
    assert!(matches!(
        conflicting.validate(),
        Err(McpConfigError::InvalidAuthentication { .. })
    ));
}
