//! Permission request types for tool execution approval.

use serde::{Deserialize, Serialize};

/// Permission request sent from the kernel to the frontend
/// when a tool execution needs user approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Identifies the tool call this permission is for.
    pub call_id: String,
    /// Human-readable message explaining what needs approval.
    pub message: String,
    /// Available permission choices for the user.
    pub options: Vec<PermissionOption>,
}

/// A single permission option the user can choose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionOption {
    /// Unique option identifier (e.g. "allow_once").
    pub id: String,
    /// Human-readable label (e.g. "Allow Once").
    pub label: String,
    /// The kind of this option determining its scope.
    pub kind: PermissionOptionKind,
}

/// Classification of a permission option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// User's decision in response to a tool approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Allow this single execution.
    AllowOnce,
    /// Allow and persist for future identical requests.
    AllowAlways,
    /// Reject this single execution.
    RejectOnce,
    /// Reject and persist for future identical requests.
    RejectAlways,
    /// Abort the entire turn.
    Abort,
}
