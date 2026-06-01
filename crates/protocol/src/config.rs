//! Session configuration types: modes, models, and configurable options.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{AgentPath, SessionId};

/// Tool-approval behaviour for a session.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Ask the user before each tool call that needs confirmation.
    #[default]
    #[serde(rename = "request_approval")]
    RequestApproval,
    /// Auto-approve all tool calls without prompting the user.
    Yolo,
}

/// enhanced approval policy for tool execution.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum AskForApproval {
    /// Ask for commands unless they are trusted by policy.
    UnlessTrusted,
    /// Run sandboxed first and ask only after sandbox failure.
    OnFailure,
    /// Let tools request approval when needed.
    #[default]
    OnRequest,
    /// Fine-grained prompt enablement for approval categories.
    Granular(GranularApprovalConfig),
    /// Never ask the user for approval.
    Never,
}

impl From<ApprovalMode> for AskForApproval {
    /// Convert legacy approval modes into enhanced approval policy.
    fn from(value: ApprovalMode) -> Self {
        match value {
            ApprovalMode::RequestApproval => AskForApproval::OnRequest,
            ApprovalMode::Yolo => AskForApproval::Never,
        }
    }
}

/// Fine-grained approval prompt enablement.
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
    /// Whether sandbox/escalation approval prompts are allowed.
    pub sandbox_approval: bool,
    /// Whether execpolicy rule prompts are allowed.
    pub rules: bool,
    /// Whether skill script approval prompts are allowed.
    #[builder(default)]
    #[serde(default)]
    pub skill_approval: bool,
    /// Whether request_permissions prompts are allowed.
    #[builder(default)]
    #[serde(default)]
    pub request_permissions: bool,
    /// Whether MCP elicitation prompts are allowed.
    pub mcp_elicitations: bool,
}

/// Per-turn context passed to every tool execution.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub struct ToolContext {
    /// Session identifier for routing session-scoped tool backends.
    pub session_id: SessionId,
    /// Working directory for this turn.
    pub cwd: PathBuf,
    /// Path of the agent executing this turn.
    pub agent_path: AgentPath,
    /// Current tool-approval mode for the session.
    pub approval_mode: ApprovalMode,
    /// Current enhanced approval policy for the session.
    #[builder(default)]
    pub approval_policy: AskForApproval,
}

/// A session mode preset (e.g. read-only, auto, full-access).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMode {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// Model info exposed to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    #[builder(default)]
    pub description: Option<String>,
    #[builder(default)]
    pub context_tokens: Option<u64>,
    #[builder(default)]
    pub max_output_tokens: Option<u64>,
}

/// A configurable option for a session (e.g. reasoning effort level).
#[derive(Debug, Clone, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub struct SessionConfigOption {
    pub id: String,
    pub name: String,
    #[builder(default)]
    pub description: Option<String>,
    pub values: Vec<SessionConfigValue>,
    #[builder(default)]
    pub current_value: Option<String>,
}

/// A selectable value within a session config option.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfigValue {
    pub id: String,
    pub label: String,
}
