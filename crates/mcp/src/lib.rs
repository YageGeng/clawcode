//! Session-scoped MCP client and tool factory.

mod client;
mod factory;

use std::collections::HashMap;

use config::{McpConfigError, McpOAuthConfig, McpServerConfig};

pub use client::{McpConnection, McpConnector, McpError, RmcpConnector};
pub use factory::{McpFactory, McpSession, SessionMcpFactory};

/// Validated transport used to establish one MCP client session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    /// Child-process stdio transport.
    Stdio {
        /// Executable name or path.
        command: String,

        /// Child-process arguments.
        args: Vec<String>,

        /// Child-process environment overrides.
        env: HashMap<String, String>,
    },

    /// MCP Streamable HTTP transport.
    StreamableHttp {
        /// MCP endpoint URL.
        url: String,

        /// Optional environment variable containing a bearer token.
        bearer_token_env: Option<String>,

        /// Custom HTTP headers.
        headers: HashMap<String, String>,
    },
}

/// Runtime-ready MCP server definition owned by a session factory.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct RuntimeMcpServer {
    /// Stable server namespace.
    pub name: String,

    /// Whether the server should connect.
    pub enabled: bool,

    /// Whether an external protocol client supplied the server.
    pub external: bool,

    /// Startup timeout in seconds.
    pub startup_timeout_sec: u64,

    /// Per-tool-call timeout in seconds.
    pub tool_timeout_sec: u64,

    /// Validated stdio or Streamable HTTP transport.
    pub transport: McpTransport,

    /// Optional OAuth settings reserved for HTTP connection setup.
    #[builder(default)]
    pub oauth: Option<McpOAuthConfig>,
}

impl TryFrom<McpServerConfig> for RuntimeMcpServer {
    type Error = McpConfigError;

    /// Validates TOML fields and converts exactly one configured transport.
    fn try_from(config: McpServerConfig) -> Result<Self, Self::Error> {
        config.validate()?;
        let transport = match (config.command.clone(), config.url.clone()) {
            (Some(command), None) => McpTransport::Stdio {
                command,
                args: config.args.clone().unwrap_or_default(),
                env: config.env.clone().unwrap_or_default(),
            },
            (None, Some(url)) => McpTransport::StreamableHttp {
                url,
                bearer_token_env: config.bearer_token_env.clone(),
                headers: config.http_headers.clone().unwrap_or_default(),
            },
            _ => {
                return Err(McpConfigError::InvalidTransport {
                    server: config.name,
                    reason: "validated transport became ambiguous".to_string(),
                });
            }
        };

        Ok(Self::builder()
            .name(config.name)
            .enabled(config.enabled)
            .external(config.external)
            .startup_timeout_sec(config.startup_timeout_sec)
            .tool_timeout_sec(config.tool_timeout_sec)
            .transport(transport)
            .oauth(config.oauth)
            .build())
    }
}
