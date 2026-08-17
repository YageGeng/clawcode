#![expect(
    deprecated,
    reason = "Modern Host compatibility retains deprecated callbacks until MRTR fully replaces them"
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

/// MCP client that only performs the 2026-07-28 discovery lifecycle.
#[derive(Debug)]
pub struct ModernClient {
    core: ClientCore,
}

impl ModernClient {
    /// Establishes exact Modern lifecycle over any rmcp-compatible transport.
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

    /// Establishes Modern lifecycle with supported nested Host callbacks.
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
                .enable_tasks()
                .build(),
        )
        .await
    }

    /// Applies one explicit Host capability declaration to the exact Modern lifecycle.
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
        .with_protocol_version(ProtocolVersion::V_2026_07_28);
        let (handler, channels) = McpLifecycleHandler::new(client_info, host);
        let running = handler
            .serve_with_lifecycle(
                transport,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            )
            .await
            .map_err(|error| Self::startup_error(&server_id, error))?;
        let core = ClientCore::from_running(
            server_id,
            McpProtocolVersion::V2026_07_28,
            ProtocolVersion::V_2026_07_28,
            running,
            channels,
        )
        .await?;
        Ok(Self { core })
    }

    /// Converts rmcp discovery negotiation failures into exact mismatch errors.
    fn startup_error(
        server_id: &McpServerId,
        error: ClientInitializeError,
    ) -> McpError {
        match error {
            ClientInitializeError::NoCompatibleProtocolVersion {
                server_supported,
                ..
            } => McpError::ProtocolMismatch {
                server_id: server_id.clone(),
                expected: McpProtocolVersion::V2026_07_28,
                actual: server_supported
                    .into_iter()
                    .map(|version| version.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            },
            other => McpError::Transport {
                server_id: server_id.clone(),
                message: other.to_string(),
            },
        }
    }
}

#[async_trait]
impl McpClient for ModernClient {
    /// Returns the Modern client's owning Server.
    fn server_id(&self) -> &McpServerId {
        self.core.server_id()
    }

    /// Returns the exact Modern protocol revision.
    fn protocol(&self) -> McpProtocolVersion {
        self.core.protocol()
    }

    /// Returns optional implementation metadata from discovery.
    fn server_info(&self) -> Option<&McpServerImplementation> {
        self.core.server_info()
    }

    /// Returns discovery-time Server capabilities.
    fn capabilities(&self) -> &McpServerCapabilities {
        self.core.declared_capabilities()
    }

    /// Discovers complete capability-gated Modern catalogs.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        let catalog = self.core.discover().await?;
        self.core.start_modern_subscription(&catalog).await?;
        Ok(catalog)
    }

    /// Subscribes to normalized Modern subscription notifications.
    fn subscribe_changes(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::McpClientEvent>> {
        Some(self.core.subscribe_changes())
    }

    /// Invokes one Modern Tool.
    async fn call_tool(
        &self,
        request: McpToolRequest,
        control: crate::McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        self.core.call_tool(request, control).await
    }

    /// Retrieves one Modern Prompt.
    async fn get_prompt(
        &self,
        request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        self.core.get_prompt(request).await
    }

    /// Reads one Modern Resource.
    async fn read_resource(
        &self,
        request: McpResourceRequest,
    ) -> Result<McpResourceResult, McpError> {
        self.core.read_resource(request).await
    }

    /// Completes one Modern Prompt or Resource Template argument.
    async fn complete(
        &self,
        request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        self.core.complete(request).await
    }

    /// Gracefully closes the Modern transport.
    async fn shutdown(&self) -> Result<(), McpError> {
        self.core.shutdown().await
    }
}
