//! Lightweight Session MCP change notifications paired with immutable snapshots.

use protocol::{McpServerId, McpSessionChange};

/// Normalized Server notification consumed by protocol-independent runtime synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpClientEvent {
    /// Complete Tool catalog may have changed.
    ToolsChanged,
    /// Complete Prompt catalog may have changed.
    PromptsChanged,
    /// Resource or Resource Template catalog may have changed.
    ResourcesChanged,
    /// One subscribed Resource changed at its original URI.
    ResourceUpdated(String),
    /// A Modern subscription stream ended and requires explicit reconnect or revalidation.
    SubscriptionEnded,
}

/// Kind of one Server-local publication consumed by its owning Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpServerChange {
    /// Server status changed without replacing its last-good catalog.
    Status,
    /// Server atomically replaced its complete catalog and status.
    Catalog,
}

/// Revision notification used to prompt consumers to read the latest Snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSessionEvent {
    /// Monotonic revision now available from the Session.
    pub revision: u64,
    /// Server responsible for a scoped change, or none for Session-wide changes.
    pub server_id: Option<McpServerId>,
    /// Stable change category.
    pub change: McpSessionChange,
}
