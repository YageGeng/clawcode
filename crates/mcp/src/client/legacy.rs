#![expect(
    deprecated,
    reason = "Legacy Roots and Sampling callbacks remain required by the supported protocol"
)]

use std::sync::Arc;

use async_trait::async_trait;
use protocol::{
    McpCatalog, McpCompletionRequest, McpCompletionResult, McpPromptRequest,
    McpPromptResult, McpProtocolVersion, McpResourceRequest, McpResourceResult,
    McpServerCapabilities, McpServerId, McpServerImplementation,
    McpToolRequest, McpToolResult, ProductIdentity,
};
use rmcp::model::{
    ClientCapabilities, ClientInfo, Implementation, ProtocolVersion,
};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RoleClient,
};
use rmcp::transport::IntoTransport;

use super::handler::McpLifecycleHandler;
use super::{McpClient, core::ClientCore};
use crate::{McpError, McpHost, host::unavailable_host};

/// MCP client that only performs the 2025-11-25 initialize lifecycle.
#[derive(Debug)]
pub struct LegacyClient {
    core: ClientCore,
}

impl LegacyClient {
    /// Establishes exact Legacy lifecycle over any rmcp-compatible transport.
    pub async fn connect<T, E, A>(
        server_id: McpServerId,
        transport: T,
    ) -> Result<Self, McpError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::connect_with_capabilities(
            server_id,
            transport,
            unavailable_host(),
            ClientCapabilities::default(),
        )
        .await
    }

    /// Establishes Legacy lifecycle with all supported Server-to-client Host callbacks.
    pub async fn connect_with_host<T, E, A>(
        server_id: McpServerId,
        transport: T,
        host: Arc<dyn McpHost>,
    ) -> Result<Self, McpError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::connect_with_capabilities(
            server_id,
            transport,
            host,
            ClientCapabilities::builder()
                .enable_roots()
                .enable_sampling()
                .enable_elicitation()
                .build(),
        )
        .await
    }

    /// Applies one explicit Host capability declaration to the exact Legacy lifecycle.
    async fn connect_with_capabilities<T, E, A>(
        server_id: McpServerId,
        transport: T,
        host: Arc<dyn McpHost>,
        capabilities: ClientCapabilities,
    ) -> Result<Self, McpError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let client_info = ClientInfo::new(
            capabilities,
            Implementation::new(
                ProductIdentity::NAME,
                env!("CARGO_PKG_VERSION"),
            ),
        )
        .with_protocol_version(ProtocolVersion::V_2025_11_25);
        let (handler, channels) = McpLifecycleHandler::new(client_info, host);
        let running = handler
            .serve_with_lifecycle(transport, ClientLifecycleMode::Initialize)
            .await
            .map_err(|error| Self::startup_error(&server_id, error))?;
        let core = ClientCore::from_running(
            server_id,
            McpProtocolVersion::V2025_11_25,
            ProtocolVersion::V_2025_11_25,
            running,
            channels,
        )
        .await?;
        Ok(Self { core })
    }

    /// Converts an rmcp initialization failure into a Server-scoped lifecycle error.
    fn startup_error(
        server_id: &McpServerId,
        error: ClientInitializeError,
    ) -> McpError {
        McpError::Transport {
            server_id: server_id.clone(),
            message: error.to_string(),
        }
    }
}

#[async_trait]
impl McpClient for LegacyClient {
    /// Returns the Legacy client's owning Server.
    fn server_id(&self) -> &McpServerId {
        self.core.server_id()
    }

    /// Returns the exact Legacy protocol revision.
    fn protocol(&self) -> McpProtocolVersion {
        self.core.protocol()
    }

    /// Returns implementation metadata from initialize.
    fn server_info(&self) -> Option<&McpServerImplementation> {
        self.core.server_info()
    }

    /// Returns initialize-time Server capabilities.
    fn capabilities(&self) -> &McpServerCapabilities {
        self.core.declared_capabilities()
    }

    /// Discovers complete capability-gated Legacy catalogs.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        self.core.discover().await
    }

    /// Subscribes to normalized Legacy list and Resource notifications.
    fn subscribe_changes(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::McpClientEvent>> {
        Some(self.core.subscribe_changes())
    }

    /// Invokes one Legacy Tool.
    async fn call_tool(
        &self,
        request: McpToolRequest,
        control: crate::McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        self.core.call_tool(request, control).await
    }

    /// Retrieves one Legacy Prompt.
    async fn get_prompt(
        &self,
        request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        self.core.get_prompt(request).await
    }

    /// Reads one Legacy Resource.
    async fn read_resource(
        &self,
        request: McpResourceRequest,
    ) -> Result<McpResourceResult, McpError> {
        self.core.read_resource(request).await
    }

    /// Completes one Legacy Prompt or Resource Template argument.
    async fn complete(
        &self,
        request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        self.core.complete(request).await
    }

    /// Gracefully closes the Legacy transport.
    async fn shutdown(&self) -> Result<(), McpError> {
        self.core.shutdown().await
    }
}
