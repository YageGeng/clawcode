//! Protocol-specific MCP clients sharing normalized capability operations.

mod catalog;
mod content;
mod core;
mod handler;
mod invocation;
mod legacy;
mod modern;
mod mrtr;
mod task;

use async_trait::async_trait;
use protocol::{
    McpCatalog, McpCompletionRequest, McpCompletionResult, McpPromptRequest,
    McpPromptResult, McpProtocolVersion, McpRequestContext, McpResourceRequest,
    McpResourceResult, McpServerCapabilities, McpServerId,
    McpServerImplementation, McpTaskStatus, McpToolRequest, McpToolResult,
};

pub use legacy::LegacyClient;
pub use modern::ModernClient;

use crate::McpClientEvent;
use crate::McpError;

/// One normalized progress update emitted by an in-flight MCP request.
#[derive(Debug, Clone, PartialEq)]
pub struct McpProgress {
    /// Monotonic work completed as reported by the Server.
    pub progress: f64,
    /// Optional expected total work.
    pub total: Option<f64>,
    /// Optional Server-authored progress message.
    pub message: Option<String>,
}

/// Receives progress without coupling the MCP client to the agent Tool registry.
pub trait McpProgressSink: Send + Sync {
    /// Publishes one latest progress notification.
    fn publish(&self, progress: McpProgress);

    /// Publishes one normalized Modern Task lifecycle snapshot.
    fn publish_task(&self, _status: McpTaskStatus) {}
}

/// Complete cancellation and deadline policy for one remote Tool request.
#[derive(typed_builder::TypedBuilder)]
pub struct McpRequestControl {
    /// Turn and trace correlation inherited from the agent Tool invocation.
    pub context: McpRequestContext,
    /// Owning Turn cancellation token.
    pub cancellation: tokio_util::sync::CancellationToken,
    /// Server lifecycle token cancelled by reconnect or Session shutdown.
    pub lifecycle: tokio_util::sync::CancellationToken,
    /// Idle deadline refreshed by valid progress notifications.
    pub idle_timeout: std::time::Duration,
    /// Absolute deadline that progress cannot extend.
    pub total_timeout: std::time::Duration,
    /// Maximum number of Modern multi-round request attempts.
    pub max_rounds: std::num::NonZeroU32,
    /// Destination for normalized progress.
    pub progress: std::sync::Arc<dyn McpProgressSink>,
}

/// Normalized client operations shared by exact Legacy and Modern lifecycles.
#[async_trait]
pub trait McpClient: Send + Sync {
    /// Returns the connected Server identifier used for request routing.
    fn server_id(&self) -> &McpServerId;

    /// Returns the exact configured and negotiated protocol revision.
    fn protocol(&self) -> McpProtocolVersion;

    /// Returns optional Server implementation identity from lifecycle metadata.
    fn server_info(&self) -> Option<&McpServerImplementation>;

    /// Returns the immutable capability gates declared during lifecycle startup.
    fn capabilities(&self) -> &McpServerCapabilities;

    /// Discovers every declared catalog family across all pagination cursors.
    async fn discover(&self) -> Result<McpCatalog, McpError>;

    /// Subscribes to normalized protocol notifications when this client exposes them.
    fn subscribe_changes(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<McpClientEvent>> {
        None
    }

    /// Invokes one Tool through its structured Server reference.
    async fn call_tool(
        &self,
        request: McpToolRequest,
        control: McpRequestControl,
    ) -> Result<McpToolResult, McpError>;

    /// Retrieves one Prompt through its structured Server reference.
    async fn get_prompt(
        &self,
        request: McpPromptRequest,
    ) -> Result<McpPromptResult, McpError>;

    /// Reads one Resource through its structured Server reference.
    async fn read_resource(
        &self,
        request: McpResourceRequest,
    ) -> Result<McpResourceResult, McpError>;

    /// Completes one Prompt or Resource Template argument.
    async fn complete(
        &self,
        request: McpCompletionRequest,
    ) -> Result<McpCompletionResult, McpError>;

    /// Gracefully closes the connected transport and background service.
    async fn shutdown(&self) -> Result<(), McpError>;
}
