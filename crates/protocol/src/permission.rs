//! Permission request types for tool execution approval.

use serde::{Deserialize, Serialize};

use crate::approvals::{
    ExecPolicyAmendment, NetworkPolicyAmendment, NetworkPolicyRuleAction,
};

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

/// User's decision in response to an approval request.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// User approved this request once.
    #[serde(alias = "allow_once")]
    Approved,
    /// User approved and wants to persist an execpolicy prefix.
    ApprovedExecpolicyAmendment {
        /// Proposed execpolicy amendment selected by the user.
        proposed_execpolicy_amendment: ExecPolicyAmendment,
    },
    /// User approved matching requests for this session.
    #[serde(alias = "allow_always")]
    ApprovedForSession,
    /// User selected a network policy amendment.
    NetworkPolicyAmendment {
        /// Proposed network policy amendment selected by the user.
        network_policy_amendment: NetworkPolicyAmendment,
    },
    /// User denied this request but the turn may continue.
    #[default]
    #[serde(alias = "reject_once", alias = "reject_always")]
    Denied,
    /// Approval timed out before a decision was received.
    TimedOut,
    /// User aborted the current turn.
    Abort,
}

impl ReviewDecision {
    /// Return a stable non-sensitive label for logs and metrics.
    #[must_use]
    pub fn to_opaque_string(&self) -> &'static str {
        match self {
            ReviewDecision::Approved => "approved",
            ReviewDecision::ApprovedExecpolicyAmendment { .. } => {
                "approved_with_amendment"
            }
            ReviewDecision::ApprovedForSession => "approved_for_session",
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } => match network_policy_amendment.action {
                NetworkPolicyRuleAction::Allow => {
                    "approved_with_network_policy_allow"
                }
                NetworkPolicyRuleAction::Deny => {
                    "denied_with_network_policy_deny"
                }
            },
            ReviewDecision::Denied => "denied",
            ReviewDecision::TimedOut => "timed_out",
            ReviewDecision::Abort => "abort",
        }
    }
}

impl From<PermissionOptionKind> for ReviewDecision {
    /// Convert a permission option kind into the unified review decision.
    fn from(value: PermissionOptionKind) -> Self {
        match value {
            PermissionOptionKind::AllowOnce => ReviewDecision::Approved,
            PermissionOptionKind::AllowAlways => {
                ReviewDecision::ApprovedForSession
            }
            PermissionOptionKind::RejectOnce
            | PermissionOptionKind::RejectAlways => ReviewDecision::Denied,
        }
    }
}
