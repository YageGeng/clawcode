//! Typed MCP configuration, lifecycle, discovery, invocation, and shutdown failures.

use std::num::NonZeroU32;

use protocol::{
    McpAuthorizationRequest, McpProtocolVersion, McpServerId, SessionId,
};

/// Failures surfaced by MCP clients and Session factories.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// A Host callback could not be completed.
    #[error("MCP Host request failed: {0}")]
    Host(String),

    /// The Server lifecycle attempted an impossible state transition.
    #[error("invalid MCP Server state transition from {from:?} to {to:?}")]
    InvalidStateTransition {
        /// Current state retained after rejection.
        from: protocol::McpServerState,
        /// Rejected next state.
        to: protocol::McpServerState,
    },

    /// Transport creation or I/O failed before a usable MCP client existed.
    #[error("MCP transport failed for server '{server_id}': {message}")]
    Transport {
        /// Server whose transport failed.
        server_id: McpServerId,
        /// Sanitized transport diagnostic.
        message: String,
    },

    /// The Server selected or advertised a revision other than the configured exact revision.
    #[error(
        "MCP protocol mismatch for server '{server_id}': expected {expected}, received {actual}"
    )]
    ProtocolMismatch {
        /// Server whose lifecycle negotiation failed.
        server_id: McpServerId,
        /// Exact configured protocol revision.
        expected: McpProtocolVersion,
        /// Server-selected revision or a stable absence marker.
        actual: String,
    },

    /// A request failed after lifecycle establishment.
    #[error("MCP service failed for server '{server_id}': {message}")]
    Service {
        /// Server whose request failed.
        server_id: McpServerId,
        /// Sanitized request diagnostic.
        message: String,
    },

    /// A caller attempted an operation the Server did not declare.
    #[error(
        "MCP server '{server_id}' does not declare capability '{capability}'"
    )]
    CapabilityUnavailable {
        /// Server lacking the requested capability.
        server_id: McpServerId,
        /// Stable capability family name.
        capability: &'static str,
    },

    /// A request reached a Server that is not currently ready to accept work.
    #[error("MCP server '{server_id}' is not ready: {state:?}")]
    ServerUnavailable {
        /// Server that rejected the new request.
        server_id: McpServerId,
        /// Current lifecycle state observed by the caller.
        state: protocol::McpServerState,
    },

    /// A Session operation referenced a Server id absent from its configuration.
    #[error("MCP server not found: {0}")]
    ServerNotFound(McpServerId),

    /// The owning Turn cancelled an in-flight Server request.
    #[error("MCP request cancelled for server '{0}'")]
    RequestCancelled(McpServerId),

    /// A Server request exceeded its configured idle deadline.
    #[error("MCP request timed out for server '{0}'")]
    RequestTimeout(McpServerId),

    /// A Modern request kept requiring input beyond its configured attempt cap.
    #[error("MCP MRTR exceeded {max_rounds} rounds for server '{server_id}'")]
    MrtrRoundsExceeded {
        /// Server that did not complete within the bounded rounds.
        server_id: McpServerId,
        /// Configured maximum number of request attempts.
        max_rounds: NonZeroU32,
    },

    /// A Modern Task reached a failed state or returned an invalid terminal payload.
    #[error("MCP Task '{task_id}' failed for server '{server_id}': {message}")]
    TaskFailed {
        /// Server that owns the Task.
        server_id: McpServerId,
        /// Server-assigned opaque Task identifier.
        task_id: String,
        /// Safe failure detail returned by the Server or local decoder.
        message: String,
    },

    /// A Modern Task reached the Server-side cancelled terminal state.
    #[error("MCP Task '{task_id}' was cancelled for server '{server_id}'")]
    TaskCancelled {
        /// Server that owns the Task.
        server_id: McpServerId,
        /// Server-assigned opaque Task identifier.
        task_id: String,
    },

    /// A structured reference was routed to a different connected Server.
    #[error("MCP request for server '{actual}' was routed to '{expected}'")]
    Routing {
        /// Server owned by this client.
        expected: McpServerId,
        /// Server carried by the structured request reference.
        actual: McpServerId,
    },

    /// rmcp returned a content variant this host cannot preserve safely.
    #[error("MCP server '{server_id}' returned unsupported content")]
    UnsupportedContent {
        /// Server that returned the unknown content variant.
        server_id: McpServerId,
    },

    /// Configured bearer-token environment variable was unavailable.
    #[error("MCP bearer token environment variable failed: {0}")]
    BearerToken(#[from] std::env::VarError),

    /// OAuth transport injection is installed by the dedicated authorization lifecycle.
    #[error("MCP OAuth transport is not initialized")]
    OAuthNotInitialized,

    /// OAuth metadata, authorization, token exchange, or refresh failed safely.
    #[error("MCP OAuth failed: {0}")]
    OAuth(String),

    /// OAuth discovery completed and browser authorization must continue through ACP.
    #[error("MCP authorization is required for server '{}'", .0.server_id)]
    AuthorizationRequired(McpAuthorizationRequest),

    /// An ACP callback did not match a retained Session/Server authorization round.
    #[error(
        "MCP authorization is not pending for server '{server_id}' in session '{session_id}'"
    )]
    AuthorizationNotPending {
        /// Session expected to own the pending browser round.
        session_id: SessionId,
        /// Server expected to own the pending browser round.
        server_id: McpServerId,
    },

    /// Connection startup exceeded its configured timeout.
    #[error("MCP startup timed out for server '{0}'")]
    StartupTimeout(McpServerId),

    /// Graceful shutdown could not join the transport task.
    #[error("MCP shutdown failed for server '{server_id}': {message}")]
    Shutdown {
        /// Server whose transport task did not stop cleanly.
        server_id: McpServerId,
        /// Sanitized join diagnostic.
        message: String,
    },

    /// Remote Tool registration conflicted with another Tool name.
    #[error("MCP tool registration failed: {0}")]
    ToolRegistration(String),
}

impl McpError {
    /// Converts rmcp authorization failures without exposing token material.
    pub(crate) fn from_oauth(error: rmcp::transport::auth::AuthError) -> Self {
        Self::OAuth(error.to_string())
    }
}
