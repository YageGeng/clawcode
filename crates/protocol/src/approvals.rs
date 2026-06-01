//! Approval protocol types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::permission::ReviewDecision;

/// Proposed execpolicy change that allows commands starting with this prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecPolicyAmendment {
    /// Command tokens that form the allow prefix.
    pub command: Vec<String>,
}

impl ExecPolicyAmendment {
    /// Create a new execpolicy amendment from command prefix tokens.
    #[must_use]
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }

    /// Return the command prefix tokens.
    #[must_use]
    pub fn command(&self) -> &[String] {
        &self.command
    }
}

impl From<Vec<String>> for ExecPolicyAmendment {
    /// Convert command prefix tokens into an amendment.
    fn from(command: Vec<String>) -> Self {
        Self { command }
    }
}

/// Network protocol attached to an approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkApprovalProtocol {
    /// Plain HTTP traffic.
    Http,
    /// HTTPS traffic or CONNECT requests.
    #[serde(alias = "https_connect", alias = "http-connect")]
    Https,
    /// SOCKS5 TCP traffic.
    Socks5Tcp,
    /// SOCKS5 UDP traffic.
    Socks5Udp,
}

/// Runtime network request context shown in approval prompts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkApprovalContext {
    /// Host that triggered the approval prompt.
    pub host: String,
    /// Protocol used by the blocked request.
    pub protocol: NetworkApprovalProtocol,
}

/// Persisted network policy action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicyRuleAction {
    /// Allow matching future network requests.
    Allow,
    /// Deny matching future network requests.
    Deny,
}

/// Proposed network policy change for a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicyAmendment {
    /// Host covered by the amendment.
    pub host: String,
    /// Action persisted for matching future requests.
    pub action: NetworkPolicyRuleAction,
}

/// Optional additional permissions requested by a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionalPermissionProfile {
    /// Whether network access is requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    /// File-system paths requested for read access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_paths: Vec<PathBuf>,
    /// File-system paths requested for write access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_paths: Vec<PathBuf>,
}

/// Parsed command summary used by approval UIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedCommand {
    /// Parsed command tokens.
    pub command: Vec<String>,
}

/// Shell command approval request delivered to clients.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct ExecApprovalRequestEvent {
    /// Tool call id that owns this approval.
    pub call_id: String,
    /// Specific approval id for nested approvals.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    /// Turn id that owns this approval.
    pub turn_id: String,
    /// Unix timestamp in milliseconds when approval started.
    pub started_at_ms: i64,
    /// Command tokens to execute.
    pub command: Vec<String>,
    /// Working directory for the command.
    pub cwd: PathBuf,
    /// Optional approval reason.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional blocked network request context.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_approval_context: Option<NetworkApprovalContext>,
    /// Optional command-prefix amendment.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    /// Optional network policy amendments.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_network_policy_amendments: Option<Vec<NetworkPolicyAmendment>>,
    /// Optional extra permissions requested by the tool.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_permissions: Option<AdditionalPermissionProfile>,
    /// Ordered decisions that clients may show.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_decisions: Option<Vec<ReviewDecision>>,
    /// Parsed command summary for display.
    #[builder(default)]
    pub parsed_cmd: Vec<ParsedCommand>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies one-shot permission options map to approval decisions.
    #[test]
    fn allow_once_option_maps_to_approved() {
        let decision = ReviewDecision::from(
            crate::permission::PermissionOptionKind::AllowOnce,
        );

        assert_eq!(decision, ReviewDecision::Approved);
    }

    /// Verifies "always" permission options become session-scoped approvals.
    #[test]
    fn allow_always_option_maps_to_approved_for_session() {
        let decision = ReviewDecision::from(
            crate::permission::PermissionOptionKind::AllowAlways,
        );

        assert_eq!(decision, ReviewDecision::ApprovedForSession);
    }

    /// Verifies execpolicy amendments serialize as command-token arrays.
    #[test]
    fn exec_policy_amendment_serializes_as_command_array() {
        let amendment = ExecPolicyAmendment::new(vec![
            "cargo".to_string(),
            "test".to_string(),
        ]);

        let encoded =
            serde_json::to_string(&amendment).expect("serialize amendment");

        assert_eq!(encoded, r#"["cargo","test"]"#);
    }
}
