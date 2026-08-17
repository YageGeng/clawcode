//! Runtime-ready MCP Server configuration derived from raw TOML values.

use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU32;
use std::time::Duration;

use config::{McpConfigError, McpOAuthConfig, McpServerConfig};
use http::{HeaderMap, HeaderName, HeaderValue};
use protocol::{McpProtocolVersion, McpServerId, McpTransportKind};
use url::Url;

/// Validated HTTP(S) URL used by MCP transports and OAuth endpoints.
#[derive(Clone, PartialEq, Eq)]
pub struct McpHttpUrl(Url);

impl McpHttpUrl {
    /// Returns the complete validated URL for transport construction.
    #[must_use]
    pub fn as_url(&self) -> &Url {
        &self.0
    }

    /// Returns the complete validated URL string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<McpHttpUrl> for std::sync::Arc<str> {
    /// Converts a validated URL into rmcp's immutable transport URI storage.
    fn from(value: McpHttpUrl) -> Self {
        std::sync::Arc::from(value.0.as_str())
    }
}

impl TryFrom<String> for McpHttpUrl {
    type Error = url::ParseError;

    /// Parses an absolute credential-free HTTP(S) URL.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let url = Url::parse(&value)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(url::ParseError::RelativeUrlWithoutBase);
        }
        Ok(Self(url))
    }
}

impl fmt::Debug for McpHttpUrl {
    /// Omits query and fragment data because endpoints may carry credential-like values there.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut redacted = self.0.clone();
        redacted.set_query(None);
        redacted.set_fragment(None);
        formatter
            .debug_tuple("McpHttpUrl")
            .field(&redacted)
            .finish()
    }
}

/// Validated custom HTTP headers whose values are never exposed through Debug.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct McpHttpHeaders(HeaderMap);

impl McpHttpHeaders {
    /// Returns one validated Header value for transport setup or inspection.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&HeaderValue> {
        self.0.get(name)
    }

    /// Iterates validated Header names and values for request-client construction.
    pub fn iter(&self) -> http::header::Iter<'_, HeaderValue> {
        self.0.iter()
    }
}

impl TryFrom<HashMap<String, String>> for McpHttpHeaders {
    type Error = http::Error;

    /// Parses every configured Header name and value before network startup.
    fn try_from(values: HashMap<String, String>) -> Result<Self, Self::Error> {
        let mut headers = HeaderMap::with_capacity(values.len());
        for (name, value) in values {
            let name = HeaderName::try_from(name)?;
            let value = HeaderValue::try_from(value)?;
            headers.insert(name, value);
        }
        Ok(Self(headers))
    }
}

impl fmt::Debug for McpHttpHeaders {
    /// Reports only Header names so diagnostics cannot reveal credentials or tenant secrets.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_set()
            .entries(self.0.keys().map(HeaderName::as_str))
            .finish()
    }
}

/// Runtime OAuth client policy with every configured endpoint parsed as HTTP(S).
#[derive(Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct McpOAuthSettings {
    /// OAuth client identifier.
    pub client_id: String,

    /// Optional confidential-client secret retained only for token exchange.
    #[builder(default)]
    pub client_secret: Option<String>,

    /// Requested authorization scopes.
    #[builder(default)]
    pub scopes: Vec<String>,

    /// Validated authorization response endpoint.
    pub redirect_uri: McpHttpUrl,

    /// Optional explicit authorization endpoint.
    #[builder(default)]
    pub authorization_url: Option<McpHttpUrl>,

    /// Optional explicit token endpoint.
    #[builder(default)]
    pub token_url: Option<McpHttpUrl>,
}

impl fmt::Debug for McpOAuthSettings {
    /// Redacts the client secret while retaining safe OAuth routing diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthSettings")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("scopes", &self.scopes)
            .field("redirect_uri", &self.redirect_uri)
            .field("authorization_url", &self.authorization_url)
            .field("token_url", &self.token_url)
            .finish()
    }
}

/// Mutually exclusive HTTP authentication policy.
#[derive(Clone, PartialEq, Eq)]
pub enum McpAuthentication {
    /// No generated Authorization Header.
    None,
    /// Static bearer token loaded from this environment variable at connection time.
    BearerTokenEnv(String),
    /// OAuth authorization and refresh policy.
    OAuth(Box<McpOAuthSettings>),
}

impl fmt::Debug for McpAuthentication {
    /// Keeps diagnostics useful without exposing credential material.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::BearerTokenEnv(environment) => formatter
                .debug_tuple("BearerTokenEnv")
                .field(environment)
                .finish(),
            Self::OAuth(settings) => {
                formatter.debug_tuple("OAuth").field(settings).finish()
            }
        }
    }
}

/// Runtime configuration for one stdio MCP transport.
#[derive(Clone, PartialEq, Eq)]
pub struct McpStdioTransport {
    /// Executable name or path.
    pub command: String,
    /// Child-process arguments.
    pub args: Vec<String>,
    /// Child-process environment overrides.
    pub env: HashMap<String, String>,
}

impl fmt::Debug for McpStdioTransport {
    /// Redacts argument and environment values that may carry credentials.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpStdioTransport")
            .field("command", &self.command)
            .field("arg_count", &self.args.len())
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Runtime configuration for one Streamable HTTP MCP transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamableHttpTransport {
    /// Validated MCP endpoint URL.
    pub url: McpHttpUrl,
    /// Validated custom HTTP headers.
    pub headers: McpHttpHeaders,
    /// Mutually exclusive authentication policy.
    pub authentication: McpAuthentication,
}

/// Validated transport used to establish one MCP client Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    /// Child-process stdio transport.
    Stdio(Box<McpStdioTransport>),
    /// MCP Streamable HTTP transport.
    StreamableHttp(Box<McpStreamableHttpTransport>),
}

/// Bounded policy for one Modern multi-round tool request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpMrtrPolicy {
    /// Maximum nested model rounds.
    pub max_rounds: NonZeroU32,
    /// Total wall-clock budget across all nested rounds.
    pub total_timeout: Duration,
}

/// Runtime-ready MCP Server definition owned by a Session factory.
#[derive(Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct RuntimeMcpServer {
    /// Stable namespace-safe Server identifier.
    pub server_id: McpServerId,
    /// Whether the Server should connect for a Session.
    pub enabled: bool,
    /// Exact MCP lifecycle used without probing or fallback.
    pub protocol: McpProtocolVersion,
    /// Maximum transport initialization time.
    pub startup_timeout: Duration,
    /// Maximum time for one MCP request.
    pub request_timeout: Duration,
    /// Bounded Modern MRTR policy.
    pub mrtr: McpMrtrPolicy,
    /// Validated stdio or Streamable HTTP transport.
    pub transport: McpTransport,
}

impl RuntimeMcpServer {
    /// Returns the public transport category without exposing transport credentials.
    #[must_use]
    pub fn transport_kind(&self) -> McpTransportKind {
        match self.transport {
            McpTransport::Stdio(_) => McpTransportKind::Stdio,
            McpTransport::StreamableHttp(_) => McpTransportKind::StreamableHttp,
        }
    }
}

impl fmt::Debug for RuntimeMcpServer {
    /// Reports runtime routing and limits through redaction-aware nested types.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeMcpServer")
            .field("server_id", &self.server_id)
            .field("enabled", &self.enabled)
            .field("protocol", &self.protocol)
            .field("startup_timeout", &self.startup_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("mrtr", &self.mrtr)
            .field("transport", &self.transport)
            .finish()
    }
}

impl TryFrom<McpServerConfig> for RuntimeMcpServer {
    type Error = McpConfigError;

    /// Converts a validated raw TOML entry into types that cannot represent ambiguous policy.
    fn try_from(config: McpServerConfig) -> Result<Self, Self::Error> {
        config.validate()?;
        let server_name = config.name.clone();
        let server_id =
            McpServerId::try_from(config.name).map_err(|error| {
                McpConfigError::InvalidName {
                    server: server_name.clone(),
                    reason: error.to_string(),
                }
            })?;
        let protocol_value =
            config
                .protocol
                .ok_or_else(|| McpConfigError::MissingProtocol {
                    server: server_name.clone(),
                })?;
        let protocol = protocol_value.parse().map_err(|_error| {
            McpConfigError::UnsupportedProtocol {
                server: server_name.clone(),
                protocol: protocol_value,
            }
        })?;
        let mrtr_max_rounds = NonZeroU32::new(config.mrtr_max_rounds).ok_or(
            McpConfigError::InvalidLimit {
                server: server_name.clone(),
                field: "mrtr_max_rounds",
                value: 0,
            },
        )?;

        let transport = match (config.command, config.url) {
            (Some(command), None) => {
                McpTransport::Stdio(Box::new(McpStdioTransport {
                    command,
                    args: config.args.unwrap_or_default(),
                    env: config.env.unwrap_or_default(),
                }))
            }
            (None, Some(url)) => {
                let url = McpHttpUrl::try_from(url).map_err(|_error| {
                    McpConfigError::InvalidHttp {
                        server: server_name.clone(),
                        reason: "url must be an absolute credential-free http(s) URL"
                            .to_string(),
                    }
                })?;
                let headers = McpHttpHeaders::try_from(
                    config.http_headers.unwrap_or_default(),
                )
                .map_err(|_error| McpConfigError::InvalidHttp {
                    server: server_name.clone(),
                    reason: "one or more HTTP Header names or values are invalid"
                        .to_string(),
                })?;
                let authentication = match (
                    config.bearer_token_env,
                    config.oauth,
                ) {
                    (Some(environment), None) => {
                        McpAuthentication::BearerTokenEnv(environment)
                    }
                    (None, Some(oauth)) => McpAuthentication::OAuth(Box::new(
                        McpOAuthSettings::try_from((
                            server_name.as_str(),
                            oauth,
                        ))?,
                    )),
                    (None, None) => McpAuthentication::None,
                    (Some(_), Some(_)) => {
                        return Err(McpConfigError::InvalidAuthentication {
                            server: server_name,
                            reason: "bearer token and OAuth cannot both be configured"
                                .to_string(),
                        });
                    }
                };
                McpTransport::StreamableHttp(Box::new(
                    McpStreamableHttpTransport {
                        url,
                        headers,
                        authentication,
                    },
                ))
            }
            _ => {
                return Err(McpConfigError::InvalidTransport {
                    server: server_name,
                    reason: "validated transport became ambiguous".to_string(),
                });
            }
        };

        Ok(Self::builder()
            .server_id(server_id)
            .enabled(config.enabled)
            .protocol(protocol)
            .startup_timeout(Duration::from_secs(config.startup_timeout_sec))
            .request_timeout(Duration::from_secs(config.request_timeout_sec))
            .mrtr(McpMrtrPolicy {
                max_rounds: mrtr_max_rounds,
                total_timeout: Duration::from_secs(
                    config.mrtr_total_timeout_sec,
                ),
            })
            .transport(transport)
            .build())
    }
}

impl TryFrom<(&str, McpOAuthConfig)> for McpOAuthSettings {
    type Error = McpConfigError;

    /// Parses every configured OAuth endpoint while preserving secret values only in memory.
    fn try_from(
        (server, config): (&str, McpOAuthConfig),
    ) -> Result<Self, Self::Error> {
        let redirect_uri = McpHttpUrl::try_from(config.redirect_uri).map_err(
            |_error| McpConfigError::InvalidAuthentication {
                server: server.to_string(),
                reason: "OAuth redirect_uri must be an absolute credential-free http(s) URL"
                    .to_string(),
            },
        )?;
        let authorization_url = config
            .authorization_url
            .map(McpHttpUrl::try_from)
            .transpose()
            .map_err(|_error| McpConfigError::InvalidAuthentication {
                server: server.to_string(),
                reason: "OAuth authorization_url must be an absolute credential-free http(s) URL"
                    .to_string(),
            })?;
        let token_url = config
            .token_url
            .map(McpHttpUrl::try_from)
            .transpose()
            .map_err(|_error| McpConfigError::InvalidAuthentication {
                server: server.to_string(),
                reason: "OAuth token_url must be an absolute credential-free http(s) URL"
                    .to_string(),
            })?;

        Ok(Self::builder()
            .client_id(config.client_id)
            .client_secret(config.client_secret)
            .scopes(config.scopes.unwrap_or_default())
            .redirect_uri(redirect_uri)
            .authorization_url(authorization_url)
            .token_url(token_url)
            .build())
    }
}
