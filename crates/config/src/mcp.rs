//! MCP server configuration loaded from the application config TOML.

use std::collections::HashMap;

use protocol::{McpProtocolVersion, McpServerId};
use serde::{Deserialize, Serialize};

/// Maximum individual MCP timeout accepted from immutable configuration.
pub const MAX_MCP_TIMEOUT_SEC: u64 = 86_400;

/// Maximum number of nested Modern MRTR model rounds accepted per request.
pub const MAX_MCP_MRTR_ROUNDS: u32 = 64;

/// Flat TOML struct for one `[[mcp_servers]]` entry.
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
    /// Unique Server identifier used in public Tool namespaces.
    pub name: String,

    /// Exact MCP revision; absence is retained so validation can report the owning Server.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub protocol: Option<String>,

    /// Whether this Server should establish a Session connection.
    #[serde(default = "default_true")]
    #[builder(default = default_true())]
    pub enabled: bool,

    /// Maximum transport initialization time.
    #[serde(default = "default_startup_timeout")]
    #[builder(default = default_startup_timeout())]
    pub startup_timeout_sec: u64,

    /// Maximum time for one MCP request.
    #[serde(default = "default_request_timeout")]
    #[builder(default = default_request_timeout())]
    pub request_timeout_sec: u64,

    /// Maximum nested model rounds for one Modern MRTR request.
    #[serde(default = "default_mrtr_max_rounds")]
    #[builder(default = default_mrtr_max_rounds())]
    pub mrtr_max_rounds: u32,

    /// Total wall-clock budget for one Modern MRTR request.
    #[serde(default = "default_mrtr_total_timeout")]
    #[builder(default = default_mrtr_total_timeout())]
    pub mrtr_total_timeout_sec: u64,

    /// Executable name or path for stdio transport.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub command: Option<String>,

    /// Child-process arguments for stdio transport.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub args: Option<Vec<String>>,

    /// Child-process environment overrides for stdio transport.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub env: Option<HashMap<String, String>>,

    /// MCP endpoint for Streamable HTTP transport.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub url: Option<String>,

    /// Environment variable containing a static HTTP bearer token.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub bearer_token_env: Option<String>,

    /// Additional Streamable HTTP request headers.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub http_headers: Option<HashMap<String, String>>,

    /// Optional OAuth 2.0 client policy for Streamable HTTP.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub oauth: Option<McpOAuthConfig>,
}

/// OAuth 2.0 configuration for an MCP Server.
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
    /// OAuth client identifier.
    pub client_id: String,

    /// Optional OAuth client secret used by confidential clients.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub client_secret: Option<String>,

    /// OAuth scopes requested during authorization.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub scopes: Option<Vec<String>>,

    /// Redirect URI receiving the authorization response.
    #[serde(default = "default_redirect_uri")]
    #[builder(default = default_redirect_uri())]
    pub redirect_uri: String,

    /// Optional explicit authorization endpoint; otherwise discovery is used.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub authorization_url: Option<String>,

    /// Optional explicit token endpoint; otherwise discovery is used.
    #[serde(default)]
    #[builder(default, setter(strip_option))]
    pub token_url: Option<String>,
}

/// Errors returned when MCP TOML cannot become an unambiguous runtime policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpConfigError {
    /// Every Server must declare an exact protocol revision.
    #[error("mcp server '{server}' must configure protocol")]
    MissingProtocol {
        /// Server whose protocol field was absent.
        server: String,
    },

    /// The configured exact protocol revision is unsupported.
    #[error("mcp server '{server}' uses unsupported protocol '{protocol}'")]
    UnsupportedProtocol {
        /// Server with the unsupported revision.
        server: String,
        /// Exact unsupported revision.
        protocol: String,
    },

    /// Server identifiers must be safe for generated Tool namespaces.
    #[error("mcp server name '{server}' is invalid: {reason}")]
    InvalidName {
        /// Invalid configured Server identifier.
        server: String,
        /// Namespace validation failure.
        reason: String,
    },

    /// Server identifiers are globally unique inside one application config.
    #[error("mcp server name '{server}' is configured more than once")]
    DuplicateName {
        /// Duplicated Server identifier.
        server: String,
    },

    /// Exactly one transport must be configured per Server.
    #[error("mcp server '{server}' transport config is invalid: {reason}")]
    InvalidTransport {
        /// Server containing the invalid transport combination.
        server: String,
        /// Stable validation reason without credentials.
        reason: String,
    },

    /// A timeout or bounded-round setting is zero or exceeds its safe maximum.
    #[error("mcp server '{server}' field '{field}' has invalid value {value}")]
    InvalidLimit {
        /// Server containing the invalid limit.
        server: String,
        /// TOML field name.
        field: &'static str,
        /// Configured numeric value.
        value: u64,
    },

    /// Authentication settings conflict or are invalid for the transport.
    #[error("mcp server '{server}' authentication is invalid: {reason}")]
    InvalidAuthentication {
        /// Server containing the invalid authentication policy.
        server: String,
        /// Stable validation reason without credential material.
        reason: String,
    },

    /// Runtime HTTP parsing rejected a URL or Header before any connection attempt.
    #[error("mcp server '{server}' HTTP config is invalid: {reason}")]
    InvalidHttp {
        /// Server containing the invalid HTTP value.
        server: String,
        /// Stable parser diagnostic without Header values.
        reason: String,
    },
}

/// Returns the default OAuth redirect URI.
fn default_redirect_uri() -> String {
    "http://localhost:19876/callback".to_string()
}

/// Returns the default enabled flag for MCP Servers.
fn default_true() -> bool {
    true
}

/// Returns the default Server startup timeout in seconds.
fn default_startup_timeout() -> u64 {
    30
}

/// Returns the default MCP request timeout in seconds.
fn default_request_timeout() -> u64 {
    120
}

/// Returns the default bounded Modern MRTR round count.
fn default_mrtr_max_rounds() -> u32 {
    8
}

/// Returns the default total Modern MRTR timeout in seconds.
fn default_mrtr_total_timeout() -> u64 {
    300
}

impl McpServerConfig {
    /// Validates one raw TOML entry without constructing transport resources.
    pub fn validate(&self) -> Result<(), McpConfigError> {
        McpServerId::try_from(self.name.as_str()).map_err(|error| {
            McpConfigError::InvalidName {
                server: self.name.clone(),
                reason: error.to_string(),
            }
        })?;

        let protocol = self.protocol.as_deref().ok_or_else(|| {
            McpConfigError::MissingProtocol {
                server: self.name.clone(),
            }
        })?;
        protocol.parse::<McpProtocolVersion>().map_err(|_error| {
            McpConfigError::UnsupportedProtocol {
                server: self.name.clone(),
                protocol: protocol.to_string(),
            }
        })?;

        for (field, value) in [
            ("startup_timeout_sec", self.startup_timeout_sec),
            ("request_timeout_sec", self.request_timeout_sec),
            ("mrtr_total_timeout_sec", self.mrtr_total_timeout_sec),
        ] {
            if value == 0 || value > MAX_MCP_TIMEOUT_SEC {
                return Err(McpConfigError::InvalidLimit {
                    server: self.name.clone(),
                    field,
                    value,
                });
            }
        }
        if self.mrtr_max_rounds == 0
            || self.mrtr_max_rounds > MAX_MCP_MRTR_ROUNDS
        {
            return Err(McpConfigError::InvalidLimit {
                server: self.name.clone(),
                field: "mrtr_max_rounds",
                value: u64::from(self.mrtr_max_rounds),
            });
        }

        let command = self.command.as_deref();
        let url = self.url.as_deref();
        if command.is_some_and(|value| value.trim().is_empty()) {
            return Err(McpConfigError::InvalidTransport {
                server: self.name.clone(),
                reason: "command must not be empty".to_string(),
            });
        }
        if url.is_some_and(|value| value.trim().is_empty()) {
            return Err(McpConfigError::InvalidTransport {
                server: self.name.clone(),
                reason: "url must not be empty".to_string(),
            });
        }

        match (command, url) {
            (Some(_), Some(_)) => {
                return Err(McpConfigError::InvalidTransport {
                    server: self.name.clone(),
                    reason: "configure either command or url, not both"
                        .to_string(),
                });
            }
            (None, None) => {
                return Err(McpConfigError::InvalidTransport {
                    server: self.name.clone(),
                    reason: "configure either command for stdio or url for streamable HTTP"
                        .to_string(),
                });
            }
            (Some(_), None) => {
                if self.bearer_token_env.is_some()
                    || self.http_headers.is_some()
                    || self.oauth.is_some()
                {
                    return Err(McpConfigError::InvalidAuthentication {
                        server: self.name.clone(),
                        reason: "HTTP authentication and headers cannot be used with stdio"
                            .to_string(),
                    });
                }
            }
            (None, Some(_)) => {
                if self.args.is_some() || self.env.is_some() {
                    return Err(McpConfigError::InvalidTransport {
                        server: self.name.clone(),
                        reason: "args and env can only be used with stdio"
                            .to_string(),
                    });
                }
            }
        }

        if self
            .bearer_token_env
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(McpConfigError::InvalidAuthentication {
                server: self.name.clone(),
                reason: "bearer_token_env must not be empty".to_string(),
            });
        }
        if self.bearer_token_env.is_some() && self.oauth.is_some() {
            return Err(McpConfigError::InvalidAuthentication {
                server: self.name.clone(),
                reason: "bearer token and OAuth cannot both be configured"
                    .to_string(),
            });
        }
        if let Some(headers) = &self.http_headers
            && headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("authorization"))
            && (self.bearer_token_env.is_some() || self.oauth.is_some())
        {
            return Err(McpConfigError::InvalidAuthentication {
                server: self.name.clone(),
                reason: "Authorization header conflicts with configured authentication"
                    .to_string(),
            });
        }
        if let Some(oauth) = &self.oauth
            && oauth.client_id.trim().is_empty()
        {
            return Err(McpConfigError::InvalidAuthentication {
                server: self.name.clone(),
                reason: "OAuth client_id must not be empty".to_string(),
            });
        }

        Ok(())
    }
}
