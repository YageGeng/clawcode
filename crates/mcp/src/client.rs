//! `ManagedClient` — single MCP server connection with tool cache.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::McpServerConfig;
use crate::McpTransportConfig;
use crate::error::McpError;
use crate::tool::McpToolInfo;

/// Minimal handler satisfying rmcp's `ClientHandler` trait.
#[derive(Clone)]
pub struct Handler;

impl rmcp::ClientHandler for Handler {}

pub(crate) type RunningService = rmcp::service::RunningService<rmcp::RoleClient, Handler>;

/// A single MCP server connection with its cached tool list.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct ManagedClient {
    pub(crate) server_name: String,
    #[builder(default, setter(strip_option))]
    pub(crate) service: Option<Arc<tokio::sync::Mutex<RunningService>>>,
    #[builder(default)]
    pub(crate) tools: Vec<McpToolInfo>,
    #[builder(default = 120)]
    pub(crate) tool_timeout_secs: u64,
}

impl ManagedClient {
    /// Connect to an MCP server, perform handshake, and cache its tool list.
    ///
    /// `auth_dir` is the clawcode MCP auth directory, used for OAuth token storage.
    pub(crate) async fn connect(
        config: &McpServerConfig,
        auth_dir: &Path,
    ) -> Result<Self, McpError> {
        use rmcp::serve_client;
        use tokio::time::timeout;

        match &config.transport {
            McpTransportConfig::Stdio { command, args, env } => {
                let cmd = crate::transport::build_stdio_command(command, args, env);
                let t = rmcp::transport::TokioChildProcess::new(cmd).map_err(|e| {
                    McpError::Startup {
                        server: config.name.clone(),
                        reason: format!("spawn failed: {e}"),
                    }
                })?;
                let running = timeout(
                    Duration::from_secs(config.startup_timeout_secs),
                    serve_client(Handler, t),
                )
                .await
                .map_err(|_e| McpError::Startup {
                    server: config.name.clone(),
                    reason: format!("timed out after {}s", config.startup_timeout_secs),
                })?
                .map_err(|e| McpError::Startup {
                    server: config.name.clone(),
                    reason: format!("handshake failed: {e}"),
                })?;
                Self::collect_tools(config, running).await
            }
            McpTransportConfig::StreamableHttp {
                url,
                bearer_token_env,
                http_headers,
            } => {
                use reqwest::header::{HeaderName, HeaderValue};

                let raw_headers =
                    crate::transport::build_http_headers(bearer_token_env, http_headers)?;
                let mut headers: HashMap<HeaderName, HeaderValue> = HashMap::new();
                for (name, value) in raw_headers.iter() {
                    headers.insert(name.clone(), value.clone());
                }

                // If OAuth is configured, try loading tokens from the file store.
                if let Some(ref _oauth) = config.oauth {
                    use oauth2::TokenResponse;
                    use rmcp::transport::auth::CredentialStore;
                    let store = crate::auth::FileCredentialStore::new(auth_dir, &config.name);
                    if let Ok(Some(creds)) = store.load().await
                        && let Some(ref token_response) = creds.token_response
                    {
                        let token = token_response.access_token().secret();
                        headers.insert(
                            HeaderName::from_static("authorization"),
                            HeaderValue::from_str(&format!("Bearer {token}"))
                                .map_err(|_e| McpError::Transport("bad bearer token".into()))?,
                        );
                    }
                }

                let cfg =
                    rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                        Arc::<str>::from(url.as_str()),
                    )
                    .custom_headers(headers);

                let t = rmcp::transport::StreamableHttpClientTransport::with_client(
                    reqwest::Client::default(),
                    cfg,
                );
                let running = timeout(
                    Duration::from_secs(config.startup_timeout_secs),
                    serve_client(Handler, t),
                )
                .await
                .map_err(|_e| McpError::Startup {
                    server: config.name.clone(),
                    reason: format!("timed out after {}s", config.startup_timeout_secs),
                })?
                .map_err(|e| McpError::Startup {
                    server: config.name.clone(),
                    reason: format!("handshake failed: {e}"),
                })?;
                Self::collect_tools(config, running).await
            }
        }
    }

    /// Connect with an injectable stdio connector for in-memory integration tests.
    #[cfg(test)]
    pub(crate) async fn connect_with_connector<T, F, E, A>(
        config: &McpServerConfig,
        auth_dir: &Path,
        stdio_connector: F,
    ) -> Result<Self, McpError>
    where
        T: rmcp::transport::IntoTransport<rmcp::RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
        F: FnOnce(tokio::process::Command) -> Result<T, McpError>,
    {
        use rmcp::serve_client;
        use tokio::time::timeout;

        match &config.transport {
            McpTransportConfig::Stdio { command, args, env } => {
                let cmd = crate::transport::build_stdio_command(command, args, env);
                let t = stdio_connector(cmd)?;
                let running = timeout(
                    Duration::from_secs(config.startup_timeout_secs),
                    serve_client(Handler, t),
                )
                .await
                .map_err(|_e| McpError::Startup {
                    server: config.name.clone(),
                    reason: format!("timed out after {}s", config.startup_timeout_secs),
                })?
                .map_err(|e| McpError::Startup {
                    server: config.name.clone(),
                    reason: format!("handshake failed: {e}"),
                })?;
                Self::collect_tools(config, running).await
            }
            McpTransportConfig::StreamableHttp { .. } => Self::connect(config, auth_dir).await,
        }
    }

    async fn collect_tools(
        config: &McpServerConfig,
        running: RunningService,
    ) -> Result<Self, McpError> {
        let tools_result = running
            .list_tools(None)
            .await
            .map_err(|e| McpError::Protocol {
                server: config.name.clone(),
                msg: format!("list_tools: {e}"),
            })?;

        let tools: Vec<McpToolInfo> = tools_result
            .tools
            .into_iter()
            .map(|t| {
                McpToolInfo::builder()
                    .server_name(config.name.clone())
                    .raw_name(t.name.to_string())
                    .callable_name(String::new())
                    .description(t.description.unwrap_or_default().to_string())
                    .input_schema(serde_json::Value::Object((*t.input_schema).clone()))
                    .build()
            })
            .collect();

        Ok(Self::builder()
            .server_name(config.name.clone())
            .service(Arc::new(tokio::sync::Mutex::new(running)))
            .tools(tools)
            .tool_timeout_secs(config.tool_timeout_secs)
            .build())
    }
}
