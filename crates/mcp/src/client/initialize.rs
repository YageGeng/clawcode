#![expect(
    deprecated,
    reason = "initialize Roots and Sampling callbacks remain required by the supported protocol"
)]

use std::sync::Arc;

use async_trait::async_trait;
use protocol::{
    McpCatalog, McpCompletionRequest, McpCompletionResult, McpPromptRequest,
    McpPromptResult, McpProtocolPolicy, McpProtocolVersion, McpResourceRequest,
    McpResourceResult, McpServerCapabilities, McpServerId,
    McpServerImplementation, McpToolRequest, McpToolResult, ProductIdentity,
};
use rmcp::model::{
    ClientCapabilities, ClientInfo, Implementation, ProtocolVersion,
};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RoleClient,
};
use rmcp::transport::IntoTransport;

use super::handler::McpLifecycleHandler;
use super::negotiation::{
    NegotiationFailure, NegotiationOutcome, NegotiationTransport,
};
use super::{McpClient, core::ClientCore};
use crate::{McpError, McpHost, host::unavailable_host};

/// MCP client that performs Pi-compatible initialize negotiation.
#[derive(Debug)]
pub struct InitializeClient {
    core: ClientCore,
}

impl InitializeClient {
    /// Establishes initialize negotiation lifecycle over any rmcp-compatible transport.
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
            McpProtocolPolicy::Auto,
        )
        .await
    }

    /// Establishes initialize lifecycle with all supported Server-to-client Host callbacks.
    pub async fn connect_with_host<T, E, A>(
        server_id: McpServerId,
        transport: T,
        host: Arc<dyn McpHost>,
        policy: McpProtocolPolicy,
    ) -> Result<Self, McpError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let mut capabilities = ClientCapabilities::builder()
            .enable_roots()
            .enable_sampling()
            .build();
        // Advertise implemented callbacks; the handler gates them by the actual server selection.
        capabilities.elicitation = Some(
            rmcp::model::ElicitationCapability::default()
                .with_form(Default::default())
                .with_url(Default::default()),
        );
        Self::connect_with_capabilities(
            server_id,
            transport,
            host,
            capabilities,
            policy,
        )
        .await
    }

    /// Applies one explicit Host capability declaration to the initialize negotiation lifecycle.
    async fn connect_with_capabilities<T, E, A>(
        server_id: McpServerId,
        transport: T,
        host: Arc<dyn McpHost>,
        capabilities: ClientCapabilities,
        policy: McpProtocolPolicy,
    ) -> Result<Self, McpError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let wire_version = match policy.requested_version() {
            McpProtocolVersion::V2025_11_25 => ProtocolVersion::V_2025_11_25,
            McpProtocolVersion::V2025_06_18 => ProtocolVersion::V_2025_06_18,
            McpProtocolVersion::V2025_03_26 => ProtocolVersion::V_2025_03_26,
            McpProtocolVersion::V2024_11_05 => ProtocolVersion::V_2024_11_05,
        };
        let client_info = ClientInfo::new(
            capabilities,
            Implementation::new(
                ProductIdentity::NAME,
                env!("CARGO_PKG_VERSION"),
            ),
        )
        .with_protocol_version(wire_version);
        // Roots retain their connection identity independently of active requests.
        let (handler, channels) =
            McpLifecycleHandler::new(server_id.clone(), client_info, host);
        let (transport, outcome) =
            NegotiationTransport::new(transport.into_transport());
        let running = handler
            .serve_with_lifecycle(transport, ClientLifecycleMode::Initialize)
            .await
            .map_err(|error| {
                let outcome = outcome
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match &*outcome {
                    NegotiationOutcome::Rejected(
                        NegotiationFailure::UnsupportedVersion(actual),
                    ) => McpError::ProtocolMismatch {
                        server_id: server_id.clone(),
                        expected: policy.requested_version(),
                        actual: actual.clone(),
                    },
                    NegotiationOutcome::Rejected(failure) => {
                        McpError::Negotiation {
                            server_id: server_id.clone(),
                            message: failure.to_string(),
                        }
                    }
                    _ => Self::startup_error(&server_id, error),
                }
            })?;
        let selected =
            {
                let outcome = outcome
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match &*outcome {
                    NegotiationOutcome::Accepted(version) => *version,
                    _ => return Err(McpError::Negotiation {
                        server_id,
                        message:
                            "initialize completed without a validated version"
                                .to_string(),
                    }),
                }
            };
        let core =
            ClientCore::from_running(server_id, selected, running, channels)
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
impl McpClient for InitializeClient {
    /// Returns the initialize client's owning Server.
    fn server_id(&self) -> &McpServerId {
        self.core.server_id()
    }

    /// Returns the exact initialize protocol revision.
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

    /// Discovers complete capability-gated initialize catalogs.
    async fn discover(&self) -> Result<McpCatalog, McpError> {
        self.core.discover().await
    }

    /// Subscribes to normalized initialize list and Resource notifications.
    fn subscribe_changes(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::McpClientEvent>> {
        Some(self.core.subscribe_changes())
    }

    /// Invokes one initialize Tool.
    async fn call_tool(
        &self,
        request: McpToolRequest,
        control: crate::McpRequestControl,
    ) -> Result<McpToolResult, McpError> {
        self.core.call_tool(request, control).await
    }

    /// Retrieves one initialize Prompt.
    async fn get_prompt(
        &self,
        request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError> {
        self.core.get_prompt(request).await
    }

    /// Reads one initialize Resource.
    async fn read_resource(
        &self,
        request: McpResourceRequest,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<McpResourceResult, McpError> {
        self.core.read_resource(request, cancellation).await
    }

    /// Completes one initialize Prompt or Resource Template argument.
    async fn complete(
        &self,
        request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError> {
        self.core.complete(request).await
    }

    /// Gracefully closes the initialize transport.
    async fn shutdown(&self) -> Result<(), McpError> {
        self.core.shutdown().await
    }
}
