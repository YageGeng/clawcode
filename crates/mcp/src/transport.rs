//! Production stdio and Streamable HTTP transport composition.

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use protocol::{
    McpAuthorizationResult, McpProtocolVersion, McpServerId, SessionId,
};
use rmcp::service::RoleClient;
use rmcp::transport::auth::AuthClient;
use rmcp::transport::{
    IntoTransport, StreamableHttpClientTransport, TokioChildProcess,
};
use store::SecretStore;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    LegacyClient, McpAuthentication, McpClient, McpError, McpHost,
    McpStreamableHttpTransport, McpTransport, ModernClient, RuntimeMcpServer,
    auth::{McpOAuthGrant, McpOAuthRuntime},
};

/// Identifies one retained OAuth round without parsing display strings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PendingAuthorizationKey {
    session_id: SessionId,
    server_id: McpServerId,
}

/// Transport connector used by Session MCP factories.
#[async_trait]
pub trait McpConnector: Send + Sync {
    /// Establishes one client through the exact configured lifecycle.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
        session_id: &SessionId,
        host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError>;

    /// Completes one retained OAuth round and establishes its authenticated client.
    async fn continue_authorization(
        &self,
        _server: &RuntimeMcpServer,
        session_id: &SessionId,
        _result: McpAuthorizationResult,
        _host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        Err(McpError::AuthorizationNotPending {
            session_id: session_id.clone(),
            server_id: _server.server_id.clone(),
        })
    }

    /// Discards retained authorization state during reconnect or Session shutdown.
    async fn cancel_authorization(
        &self,
        _session_id: &SessionId,
        _server_id: &McpServerId,
    ) {
    }
}

/// Production connector for child-process stdio and Streamable HTTP.
#[derive(Clone)]
pub struct RmcpConnector {
    secret_store: Option<Arc<dyn SecretStore>>,
    pending_authorizations: Arc<
        tokio::sync::Mutex<
            BTreeMap<
                PendingAuthorizationKey,
                rmcp::transport::auth::AuthorizationSession,
            >,
        >,
    >,
}

impl RmcpConnector {
    /// Creates a production connector with durable OAuth credential storage.
    #[must_use]
    pub fn new(secret_store: Arc<dyn SecretStore>) -> Self {
        Self {
            secret_store: Some(secret_store),
            pending_authorizations: Arc::new(tokio::sync::Mutex::new(
                BTreeMap::new(),
            )),
        }
    }

    /// Applies one exact protocol lifecycle to a prepared transport.
    async fn connect_transport<T, E, A>(
        server: &RuntimeMcpServer,
        transport: T,
        host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        match server.protocol {
            McpProtocolVersion::V2025_11_25 => Ok(Arc::new(
                LegacyClient::connect_with_host(
                    server.server_id.clone(),
                    transport,
                    host,
                )
                .await?,
            )
                as Arc<dyn McpClient>),
            McpProtocolVersion::V2026_07_28 => Ok(Arc::new(
                ModernClient::connect_with_host(
                    server.server_id.clone(),
                    transport,
                    host,
                )
                .await?,
            )
                as Arc<dyn McpClient>),
        }
    }

    /// Builds the authenticated Streamable HTTP transport shared by restore and continuation.
    async fn connect_oauth_transport(
        server: &RuntimeMcpServer,
        config: &McpStreamableHttpTransport,
        manager: rmcp::transport::auth::AuthorizationManager,
        host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        let custom_headers = config
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<HashMap<_, _>>();
        let transport_config = rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(config.url.clone())
            .custom_headers(custom_headers);
        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| McpError::Transport {
                server_id: server.server_id.clone(),
                message: error.to_string(),
            })?;
        let transport = StreamableHttpClientTransport::with_client(
            AuthClient::new(http_client, manager),
            transport_config,
        );
        Self::connect_transport(server, transport, host).await
    }
}

impl Default for RmcpConnector {
    /// Creates a connector suitable for non-OAuth transports and protocol tests.
    fn default() -> Self {
        Self {
            secret_store: None,
            pending_authorizations: Arc::new(tokio::sync::Mutex::new(
                BTreeMap::new(),
            )),
        }
    }
}

#[async_trait]
impl McpConnector for RmcpConnector {
    /// Builds one transport, applies its exact lifecycle, and enforces startup timeout.
    async fn connect(
        &self,
        server: &RuntimeMcpServer,
        session_id: &SessionId,
        host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        let startup = async {
            match &server.transport {
                McpTransport::Stdio(config) => {
                    let mut command =
                        tokio::process::Command::new(&config.command);
                    command.args(&config.args).envs(&config.env);
                    let (transport, stderr) =
                        TokioChildProcess::builder(command)
                            .stderr(Stdio::piped())
                            .spawn()
                            .map_err(|error| McpError::Transport {
                                server_id: server.server_id.clone(),
                                message: error.to_string(),
                            })?;
                    if let Some(stderr) = stderr {
                        let server_id = server.server_id.clone();
                        tokio::spawn(async move {
                            let mut lines = BufReader::new(stderr).lines();
                            let mut line_count = 0_u64;
                            while let Ok(Some(_line)) = lines.next_line().await
                            {
                                line_count = line_count.saturating_add(1);
                            }
                            tracing::debug!(
                                "MCP server '{}' stderr stream closed after {} lines",
                                server_id,
                                line_count
                            );
                        });
                    }
                    Self::connect_transport(server, transport, host).await
                }
                McpTransport::StreamableHttp(config) => {
                    let custom_headers = config
                        .headers
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect::<HashMap<_, _>>();
                    let mut transport_config = rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(config.url.clone())
                        .custom_headers(custom_headers);
                    match &config.authentication {
                        McpAuthentication::None => {
                            let transport =
                                StreamableHttpClientTransport::from_config(
                                    transport_config,
                                );
                            Self::connect_transport(server, transport, host)
                                .await
                        }
                        McpAuthentication::BearerTokenEnv(environment) => {
                            transport_config = transport_config
                                .auth_header(std::env::var(environment)?);
                            let transport =
                                StreamableHttpClientTransport::from_config(
                                    transport_config,
                                );
                            Self::connect_transport(server, transport, host)
                                .await
                        }
                        McpAuthentication::OAuth(settings) => {
                            let secrets = self
                                .secret_store
                                .as_ref()
                                .map(Arc::clone)
                                .ok_or(McpError::OAuthNotInitialized)?;
                            let manager = McpOAuthRuntime::builder()
                                .server_id(server.server_id.clone())
                                .session_id(session_id.clone())
                                .resource_url(config.url.clone())
                                .settings(settings.as_ref().clone())
                                .secrets(secrets)
                                .build()
                                .authorize()
                                .await?;
                            match manager {
                                McpOAuthGrant::Ready(manager) => {
                                    Self::connect_oauth_transport(
                                        server, config, manager, host,
                                    )
                                    .await
                                }
                                McpOAuthGrant::Pending { session, request } => {
                                    let key = PendingAuthorizationKey {
                                        session_id: session_id.clone(),
                                        server_id: server.server_id.clone(),
                                    };
                                    self.pending_authorizations
                                        .lock()
                                        .await
                                        .insert(key, session);
                                    Err(McpError::AuthorizationRequired(
                                        request,
                                    ))
                                }
                            }
                        }
                    }
                }
            }
        };

        tokio::time::timeout(server.startup_timeout, startup)
            .await
            .map_err(|_elapsed| {
                McpError::StartupTimeout(server.server_id.clone())
            })?
    }

    /// Validates a browser callback against retained PKCE state before connecting.
    async fn continue_authorization(
        &self,
        server: &RuntimeMcpServer,
        session_id: &SessionId,
        result: McpAuthorizationResult,
        host: Arc<dyn McpHost>,
    ) -> Result<Arc<dyn McpClient>, McpError> {
        let key = PendingAuthorizationKey {
            session_id: session_id.clone(),
            server_id: server.server_id.clone(),
        };
        let pending = self
            .pending_authorizations
            .lock()
            .await
            .remove(&key)
            .ok_or_else(|| McpError::AuthorizationNotPending {
                session_id: session_id.clone(),
                server_id: server.server_id.clone(),
            })?;
        pending
            .handle_callback_url(&result.response_uri)
            .await
            .map_err(McpError::from_oauth)?;
        let McpTransport::StreamableHttp(config) = &server.transport else {
            return Err(McpError::AuthorizationNotPending {
                session_id: session_id.clone(),
                server_id: server.server_id.clone(),
            });
        };
        tokio::time::timeout(
            server.startup_timeout,
            Self::connect_oauth_transport(
                server,
                config,
                pending.auth_manager,
                host,
            ),
        )
        .await
        .map_err(|_elapsed| {
            McpError::StartupTimeout(server.server_id.clone())
        })?
    }

    /// Removes one exact pending browser round without affecting other Sessions.
    async fn cancel_authorization(
        &self,
        session_id: &SessionId,
        server_id: &McpServerId,
    ) {
        self.pending_authorizations.lock().await.remove(
            &PendingAuthorizationKey {
                session_id: session_id.clone(),
                server_id: server_id.clone(),
            },
        );
    }
}

impl std::fmt::Debug for RmcpConnector {
    /// Reports whether OAuth persistence exists without exposing its backend.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RmcpConnector")
            .field("secret_store", &self.secret_store.is_some())
            .finish()
    }
}
