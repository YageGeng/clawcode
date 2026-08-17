use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    McpConnector, McpError, McpServerRuntime, McpSession, McpSessionRequest,
    RuntimeMcpServer,
};

/// Factory interface used by Kernel construction for Session-scoped MCP ownership.
#[async_trait]
pub trait McpFactory: Send + Sync {
    /// Concurrently bootstraps configured Servers and isolates Server-local failures.
    async fn create(
        &self,
        request: McpSessionRequest,
    ) -> Result<McpSession, McpError>;
}

/// Application-lifetime factory holding only immutable Server config and connector policy.
pub struct SessionMcpFactory {
    servers: Vec<RuntimeMcpServer>,
    connector: Arc<dyn McpConnector>,
}

impl SessionMcpFactory {
    /// Creates an MCP factory from validated Servers and one transport connector.
    #[must_use]
    pub fn new(
        servers: Vec<RuntimeMcpServer>,
        connector: Arc<dyn McpConnector>,
    ) -> Self {
        Self { servers, connector }
    }
}

#[async_trait]
impl McpFactory for SessionMcpFactory {
    /// Starts enabled Servers concurrently and returns their settled status views.
    async fn create(
        &self,
        request: McpSessionRequest,
    ) -> Result<McpSession, McpError> {
        let session_id = request.session_id.clone();
        let servers = futures::future::join_all(
            self.servers.iter().cloned().map(|config| {
                McpServerRuntime::bootstrap(
                    config,
                    Arc::clone(&self.connector),
                    session_id.clone(),
                    Arc::clone(&request.host),
                    request.shutdown.clone(),
                )
            }),
        )
        .await;
        Ok(McpSession::new(request, servers))
    }
}
