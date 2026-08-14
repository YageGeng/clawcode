//! Tool-approval configuration owned by the configuration boundary.

use serde::{Deserialize, Serialize};

/// Legacy high-level approval behavior preserved for TOML compatibility.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Ask before tool calls that require confirmation.
    #[default]
    #[serde(rename = "request_approval")]
    RequestApproval,

    /// Never ask before tool execution.
    Yolo,
}

/// Fine-grained approval categories enabled for one runtime session.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct GranularApprovalConfig {
    /// Whether sandbox escalation prompts are enabled.
    pub sandbox_approval: bool,

    /// Whether persistent rule prompts are enabled.
    pub rules: bool,

    /// Whether skill execution prompts are enabled.
    #[builder(default)]
    #[serde(default)]
    pub skill_approval: bool,

    /// Whether explicit permission-request prompts are enabled.
    #[builder(default)]
    #[serde(default)]
    pub request_permissions: bool,

    /// Whether MCP elicitation prompts are enabled.
    pub mcp_elicitations: bool,
}

/// Effective approval policy consumed by kernel and tool factories.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum AskForApproval {
    /// Ask unless a trusted policy already allows the operation.
    UnlessTrusted,

    /// Ask only after a sandboxed attempt fails.
    OnFailure,

    /// Ask when a tool or policy explicitly requests approval.
    #[default]
    OnRequest,

    /// Use category-level approval prompt settings.
    Granular(GranularApprovalConfig),

    /// Never ask for approval.
    Never,
}

impl From<ApprovalMode> for AskForApproval {
    /// Converts the legacy approval setting into the effective policy.
    fn from(value: ApprovalMode) -> Self {
        match value {
            ApprovalMode::RequestApproval => Self::OnRequest,
            ApprovalMode::Yolo => Self::Never,
        }
    }
}
