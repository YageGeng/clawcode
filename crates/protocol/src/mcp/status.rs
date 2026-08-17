use serde::{Deserialize, Serialize};

use crate::{
    McpCapabilityCounts, McpCatalog, McpOAuthStatus, McpProtocolVersion,
    McpServerId, McpTransportKind, SessionId, TimestampMs,
};

/// Kind of immutable Session MCP view change published outside the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpSessionChange {
    /// Initial concurrent bootstrap settled and published one consistent view.
    Bootstrap,
    /// One Server lifecycle status changed.
    ServerStatus,
    /// One or more capability catalogs changed atomically.
    Catalog,
    /// Explicit Session shutdown removed active catalogs and stopped Servers.
    Shutdown,
}

/// ACP notification that tells clients which atomic Session snapshot revision to read.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpSessionRevisionNotification {
    /// Session whose MCP snapshot changed.
    pub session_id: SessionId,
    /// Monotonic snapshot revision now available.
    pub revision: u64,
    /// Server responsible for the change, or none for Session-wide changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub server_id: Option<McpServerId>,
    /// Stable category used to decide whether a refresh is relevant.
    pub change: McpSessionChange,
    /// Precision-safe time when ACP published this notification.
    pub timestamp_ms: TimestampMs,
}

/// Session-scoped lifecycle state for one configured MCP Server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpServerState {
    /// Configuration intentionally disabled this Server.
    Disabled,
    /// Transport startup is in progress.
    Starting,
    /// Exact protocol negotiation is in progress.
    Negotiating,
    /// Initial capability discovery is in progress.
    Discovering,
    /// The Server and its last published catalogs are healthy.
    Ready,
    /// A refresh failed while the last successful catalogs remain available.
    Degraded,
    /// Startup or an unrecoverable Server operation failed.
    Failed,
    /// Explicit reconnect or Session shutdown is closing the Server.
    Stopping,
    /// All Server resources have stopped.
    Stopped,
}

/// Lifecycle stage that produced a safe MCP failure summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpFailureStage {
    /// Transport construction or connection.
    Transport,
    /// Exact protocol negotiation.
    Negotiation,
    /// Initial capability discovery.
    Discovery,
    /// Dynamic capability synchronization.
    Synchronization,
    /// Static or OAuth authentication.
    Authentication,
    /// Tool, Prompt, Resource, Completion, or Host invocation.
    Invocation,
    /// Explicit Server or Session resource cleanup.
    Shutdown,
}

/// Per-capability revisions published by one MCP Server.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct McpCatalogRevisions {
    /// Tool catalog revision.
    pub tools: u64,
    /// Prompt catalog revision.
    pub prompts: u64,
    /// Resource catalog revision.
    pub resources: u64,
    /// Resource Template catalog revision.
    pub resource_templates: u64,
}

/// Server implementation identity returned by MCP negotiation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerImplementation {
    /// Server implementation name.
    pub name: String,
    /// Server implementation version.
    pub version: String,
}

/// Safe structured failure retained in an MCP Server status snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerFailure {
    /// Lifecycle stage that failed.
    pub stage: McpFailureStage,
    /// Human-readable message with credentials removed.
    pub message: String,
    /// Unix millisecond time when the failure occurred.
    pub occurred_at_ms: TimestampMs,
}

/// Complete read-only status for one configured Session-scoped MCP Server.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    /// Stable configuration and routing identity.
    pub server_id: McpServerId,
    /// Exact configured protocol version.
    pub protocol: McpProtocolVersion,
    /// Configured transport kind.
    pub transport: McpTransportKind,
    /// Current lifecycle state.
    pub state: McpServerState,
    /// Negotiated Server implementation identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub implementation: Option<McpServerImplementation>,
    /// Last published catalog revisions.
    pub revisions: McpCatalogRevisions,
    /// Last published catalog entity counts.
    pub counts: McpCapabilityCounts,
    /// Latest safe failure when the Server is degraded or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub failure: Option<McpServerFailure>,
    /// Current OAuth interaction state without credentials.
    pub oauth: McpOAuthStatus,
    /// Unix millisecond time of the latest status transition.
    pub updated_at_ms: TimestampMs,
}

/// Immutable consistent status view for one Session's configured MCP Servers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSessionSnapshot {
    /// Monotonic Session MCP snapshot revision.
    pub revision: u64,
    /// Server statuses kept in configuration order.
    pub servers: Vec<McpServerStatus>,
    /// Combined immutable catalog published atomically with Server statuses.
    #[serde(default)]
    pub catalog: McpCatalog,
}
