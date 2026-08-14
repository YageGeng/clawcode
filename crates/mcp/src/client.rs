use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use http::{HeaderName, HeaderValue};
use protocol::{McpCallResult, McpToolDescriptor};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};

use crate::{McpTransport, RuntimeMcpServer};

/// MCP startup, discovery, and call failures.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Child-process transport could not start.
    #[error("MCP stdio transport failed: {0}")]
    Stdio(String),
    /// HTTP header configuration was invalid.
    #[error("MCP HTTP header is invalid: {0}")]
    InvalidHeader(String),
    /// Configured bearer-token environment variable was unavailable.
    #[error("MCP bearer token environment variable failed: {0}")]
    BearerToken(#[from] std::env::VarError),
    /// Connection startup exceeded its configured timeout.
    #[error("MCP startup timed out for server {0}")]
    StartupTimeout(String),
    /// rmcp service operation failed.
    #[error("MCP service failed: {0}")]
    Service(String),
    /// Remote tool registration conflicted with an existing name.
    #[error("MCP tool registration failed: {0}")]
    ToolRegistration(String),
}

/// Connected MCP service abstraction used by namespaced tool adapters.
#[async_trait]
pub trait McpConnection: Send + Sync {
    /// Discovers all tools exposed by the connected server.
    async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>, McpError>;

    /// Executes one remote tool with validated object arguments.
    async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<McpCallResult, McpError>;
}

/// Transport connector used by session MCP factories.
#[async_trait]
pub trait McpConnector: Send + Sync {
    /// Establishes one initialized MCP client connection.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
    ) -> Result<Arc<dyn McpConnection>, McpError>;
}

/// Production connector implemented with rmcp stdio and Streamable HTTP transports.
#[derive(Debug, Clone, Copy, Default)]
pub struct RmcpConnector;

#[async_trait]
impl McpConnector for RmcpConnector {
    /// Connects and initializes one rmcp client using the selected transport.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
    ) -> Result<Arc<dyn McpConnection>, McpError> {
        let startup = async {
            match &server.transport {
                McpTransport::Stdio { command, args, env } => {
                    let mut process = tokio::process::Command::new(command);
                    process.args(args).envs(env);
                    let transport = TokioChildProcess::new(process)
                        .map_err(|error| McpError::Stdio(error.to_string()))?;
                    let service = ().serve(transport).await.map_err(|error| {
                        McpError::Service(error.to_string())
                    })?;
                    Ok::<_, McpError>(RmcpConnection { service })
                }
                McpTransport::StreamableHttp {
                    url,
                    bearer_token_env,
                    headers,
                } => {
                    let custom_headers = headers
                        .iter()
                        .map(|(name, value)| {
                            let name = HeaderName::try_from(name.as_str())
                                .map_err(|error| {
                                    McpError::InvalidHeader(error.to_string())
                                })?;
                            let value = HeaderValue::try_from(value.as_str())
                                .map_err(|error| {
                                McpError::InvalidHeader(error.to_string())
                            })?;
                            Ok((name, value))
                        })
                        .collect::<Result<HashMap<_, _>, McpError>>()?;
                    let mut transport_config = rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(url.clone())
                        .custom_headers(custom_headers);
                    if let Some(environment) = bearer_token_env {
                        transport_config = transport_config
                            .auth_header(std::env::var(environment)?);
                    }
                    let transport = StreamableHttpClientTransport::from_config(
                        transport_config,
                    );
                    let service = ().serve(transport).await.map_err(|error| {
                        McpError::Service(error.to_string())
                    })?;
                    Ok(RmcpConnection { service })
                }
            }
        };
        let connection = tokio::time::timeout(
            Duration::from_secs(server.startup_timeout_sec),
            startup,
        )
        .await
        .map_err(|_elapsed| McpError::StartupTimeout(server.name.clone()))??;
        Ok(Arc::new(connection))
    }
}

struct RmcpConnection {
    service: RunningService<RoleClient, ()>,
}

#[async_trait]
impl McpConnection for RmcpConnection {
    /// Discovers remote tools through rmcp pagination helpers.
    async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>, McpError> {
        self.service
            .list_all_tools()
            .await
            .map_err(|error| McpError::Service(error.to_string()))?
            .into_iter()
            .map(|tool| {
                Ok(McpToolDescriptor {
                    name: tool.name.into_owned(),
                    description: tool
                        .description
                        .map_or_else(String::new, std::borrow::Cow::into_owned),
                    input_schema: serde_json::Value::Object(
                        (*tool.input_schema).clone(),
                    ),
                })
            })
            .collect()
    }

    /// Executes one remote tool and normalizes text and structured content.
    async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<McpCallResult, McpError> {
        let result = self
            .service
            .call_tool(
                CallToolRequestParams::new(name.to_string())
                    .with_arguments(arguments),
            )
            .await
            .map_err(|error| McpError::Service(error.to_string()))?;
        let mut content = result
            .content
            .iter()
            .map(|block| {
                block.as_text().map_or_else(
                    || {
                        serde_json::to_string(block).unwrap_or_else(|_| {
                            "<unsupported MCP content>".to_string()
                        })
                    },
                    |text| text.text.clone(),
                )
            })
            .collect::<Vec<_>>();
        if let Some(structured) = result.structured_content {
            content.push(structured.to_string());
        }
        Ok(McpCallResult {
            content,
            is_error: result.is_error.unwrap_or(false),
        })
    }
}
