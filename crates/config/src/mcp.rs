//! MCP server configuration loaded from the application config TOML.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Flat TOML struct for `[[mcp_servers]]`.
#[derive(
    Debug,
    Clone,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    typed_builder::TypedBuilder,
)]
pub struct McpServerConfig {
    /// Unique server name used in MCP tool names and auth file names.
    pub name: String,

    #[serde(default = "default_true")]
    #[builder(default = default_true())]
    pub enabled: bool,

    /// Whether this server is registered by an external caller at runtime.
    #[serde(default)]
    #[builder(default)]
    pub external: bool,

    #[serde(default = "default_startup_timeout")]
    #[builder(default = default_startup_timeout())]
    pub startup_timeout_sec: u64,

    #[serde(default = "default_tool_timeout")]
    #[builder(default = default_tool_timeout())]
    pub tool_timeout_sec: u64,

    // Stdio
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub command: Option<String>,
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub env: Option<HashMap<String, String>>,

    // StreamableHTTP
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub url: Option<String>,
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub bearer_token_env: Option<String>,
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub http_headers: Option<HashMap<String, String>>,

    // OAuth
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub oauth: Option<McpOAuthConfig>,
}

/// OAuth 2.0 configuration for an MCP server.
#[derive(
    Debug,
    Clone,
    Deserialize,
    Serialize,
    PartialEq,
    Eq,
    typed_builder::TypedBuilder,
)]
pub struct McpOAuthConfig {
    /// OAuth client ID.
    pub client_id: String,
    /// OAuth client secret (optional, for client_credentials grant).
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub client_secret: Option<String>,
    /// OAuth scopes to request.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub scopes: Option<Vec<String>>,
    /// Redirect URI for authorization code flow.
    #[serde(default = "default_redirect_uri")]
    #[builder(default = default_redirect_uri())]
    pub redirect_uri: String,
    /// Authorization server URL. If not set, auto-discovered.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub authorization_url: Option<String>,
    /// Token endpoint URL. If not set, auto-discovered.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub token_url: Option<String>,
}

/// Errors returned when MCP TOML config cannot map to one runtime transport.
#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    /// Server names must be non-empty because they become tool-name namespaces.
    #[error("mcp server name must not be empty")]
    EmptyName,
    /// Exactly one transport must be configured per server.
    #[error("mcp server {server} transport config is invalid: {reason}")]
    InvalidTransport { server: String, reason: String },
}

/// Return the default OAuth redirect URI.
fn default_redirect_uri() -> String {
    "http://localhost:19876/callback".to_string()
}

/// Return the default enabled flag for MCP servers.
fn default_true() -> bool {
    true
}

/// Return the default server startup timeout in seconds.
fn default_startup_timeout() -> u64 {
    30
}

/// Return the default MCP tool-call timeout in seconds.
fn default_tool_timeout() -> u64 {
    120
}

impl McpServerConfig {
    /// Validate that one MCP server entry has a stable name and one transport.
    pub fn validate(&self) -> Result<(), McpConfigError> {
        if self.name.trim().is_empty() {
            return Err(McpConfigError::EmptyName);
        }

        let command_is_present = present_string(&self.command);
        let url_is_present = present_string(&self.url);

        if self.command.is_some() && !command_is_present {
            return Err(McpConfigError::InvalidTransport {
                server: self.name.clone(),
                reason: "command must not be empty".to_string(),
            });
        }
        if self.url.is_some() && !url_is_present {
            return Err(McpConfigError::InvalidTransport {
                server: self.name.clone(),
                reason: "url must not be empty".to_string(),
            });
        }

        match (command_is_present, url_is_present) {
            (true, true) => Err(McpConfigError::InvalidTransport {
                server: self.name.clone(),
                reason: "configure either command or url, not both".to_string(),
            }),
            (false, false) => Err(McpConfigError::InvalidTransport {
                server: self.name.clone(),
                reason: "configure either command for stdio or url for streamable HTTP"
                    .to_string(),
            }),
            (true, false) if self.oauth.is_some() => {
                Err(McpConfigError::InvalidTransport {
                    server: self.name.clone(),
                    reason: "oauth is only supported for streamable HTTP servers"
                        .to_string(),
                })
            }
            _ => Ok(()),
        }
    }
}

/// Return whether an optional string contains non-whitespace content.
fn present_string(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TOML MCP server configs default to non-external servers.
    #[test]
    fn mcp_server_config_external_defaults_false() {
        let config: McpServerConfig = toml::from_str(
            r#"
name = "server"
command = "echo"
"#,
        )
        .expect("config should deserialize");

        assert!(!config.external);
    }
}
