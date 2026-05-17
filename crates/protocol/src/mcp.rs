//! MCP configuration types shared across crates.

use std::collections::HashMap;
use std::path::PathBuf;

/// Runtime-ready MCP server configuration.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct McpServerConfig {
    /// Unique server name used as the MCP tool namespace.
    pub name: String,
    /// Concrete transport selected from user configuration.
    pub transport: McpTransportConfig,
    /// Whether this server should be started.
    #[builder(default = true)]
    pub enabled: bool,
    /// Whether this server was registered dynamically by an external caller.
    #[builder(default)]
    pub external: bool,
    /// Handshake timeout in seconds (default 30).
    #[builder(default = 30)]
    pub startup_timeout_secs: u64,
    /// Per-tool-call timeout in seconds (default 120).
    #[builder(default = 120)]
    pub tool_timeout_secs: u64,
    /// OAuth parameters (StreamableHTTP only).
    #[builder(default, setter(strip_option))]
    pub oauth: Option<McpOAuthParams>,
}

/// Transport layer configuration for an MCP server.
#[derive(Debug, Clone)]
pub enum McpTransportConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        cwd: Option<PathBuf>,
    },
    StreamableHttp {
        url: String,
        bearer_token_env: Option<String>,
        http_headers: HashMap<String, String>,
    },
}

/// Per-server MCP startup outcome.
#[derive(Debug, Clone)]
pub enum McpStartupStatus {
    Ready,
    Failed { reason: String },
}

/// OAuth 2.0 parameters for an MCP server connection.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct McpOAuthParams {
    /// OAuth client ID.
    pub client_id: String,
    /// OAuth client secret for confidential clients.
    #[builder(default, setter(strip_option))]
    pub client_secret: Option<String>,
    /// OAuth scopes to request.
    #[builder(default)]
    pub scopes: Vec<String>,
    /// Redirect URI for authorization-code flows.
    pub redirect_uri: String,
    /// Authorization endpoint override.
    #[builder(default, setter(strip_option))]
    pub authorization_url: Option<String>,
    /// Token endpoint override.
    #[builder(default, setter(strip_option))]
    pub token_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal stdio transport for runtime MCP config tests.
    fn stdio_transport() -> McpTransportConfig {
        McpTransportConfig::Stdio {
            command: "server".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
        }
    }

    /// Runtime MCP server configs default to non-external servers.
    #[test]
    fn mcp_server_config_external_defaults_false() {
        let config = McpServerConfig::builder()
            .name("server".to_string())
            .transport(stdio_transport())
            .build();

        assert!(!config.external);
    }
}
