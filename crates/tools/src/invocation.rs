//! Tool invocation envelope and approval metadata traits.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocol::{ToolContext, ToolStreamItem, TurnId};

/// Serializable key used for session-scoped approval reuse.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApprovalCacheKey {
    /// Stable approval key category.
    pub kind: String,
    /// Working directory attached to the approval.
    pub cwd: PathBuf,
    /// Tool-defined key payload.
    pub value: serde_json::Value,
}

impl ApprovalCacheKey {
    /// Create a session approval cache key.
    #[must_use]
    pub fn new(
        kind: impl Into<String>,
        cwd: PathBuf,
        value: serde_json::Value,
    ) -> Self {
        Self {
            kind: kind.into(),
            cwd,
            value,
        }
    }
}

/// Object-safe approval metadata attached to a tool invocation.
pub trait ToolApprovalInvocation: std::fmt::Debug + Send + Sync {
    /// Return cache keys that can reuse session approvals for this invocation.
    fn cache_keys(&self, cwd: &Path) -> Vec<ApprovalCacheKey>;

    /// Return an optional execpolicy amendment proposed by this invocation.
    fn proposed_execpolicy_amendment(
        &self,
    ) -> Option<protocol::ExecPolicyAmendment> {
        None
    }
}

/// Unified fact record for a model-requested tool call.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct ToolInvocation {
    /// Tool call id from the provider.
    pub call_id: String,
    /// Optional nested approval id.
    #[builder(default)]
    pub approval_id: Option<String>,
    /// Turn id that owns this invocation.
    #[builder(default)]
    pub turn_id: Option<TurnId>,
    /// Tool name selected by the model.
    pub tool_name: String,
    /// Raw JSON arguments emitted by the provider.
    pub raw_arguments: serde_json::Value,
    /// Working directory for this invocation.
    pub cwd: PathBuf,
    /// Tool-specific approval metadata.
    pub approval: Arc<dyn ToolApprovalInvocation>,
}

impl ToolInvocation {
    /// Build a generic invocation for tools that have not provided typed metadata yet.
    #[must_use]
    pub fn generic(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        raw_arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Self {
        let tool_name = tool_name.into();
        Self {
            call_id: call_id.into(),
            approval_id: None,
            turn_id: None,
            tool_name: tool_name.clone(),
            raw_arguments: raw_arguments.clone(),
            cwd: ctx.cwd.clone(),
            approval: Arc::new(GenericApprovalInvocation {
                tool_name,
                raw_arguments,
            }),
        }
    }
}

/// Fallback approval metadata.
#[derive(Debug, Clone, Serialize)]
pub struct GenericApprovalInvocation {
    /// Tool name used by the fallback approval key.
    pub tool_name: String,
    /// Raw arguments used by the fallback approval key.
    pub raw_arguments: serde_json::Value,
}

impl ToolApprovalInvocation for GenericApprovalInvocation {
    /// Return raw tool name and arguments as the fallback session approval key.
    fn cache_keys(&self, cwd: &Path) -> Vec<ApprovalCacheKey> {
        vec![ApprovalCacheKey::new(
            "generic",
            cwd.to_path_buf(),
            serde_json::json!({
                "tool_name": self.tool_name.clone(),
                "raw_arguments": self.raw_arguments.clone(),
            }),
        )]
    }
}

/// Tool execution output stream type.
pub type ToolExecution = std::pin::Pin<
    Box<dyn futures::stream::Stream<Item = ToolStreamItem> + Send>,
>;

/// Error returned while constructing a tool invocation.
#[derive(Debug, thiserror::Error)]
pub enum ToolInvocationError {
    /// Tool arguments could not be parsed.
    #[error("invalid tool arguments: {0}")]
    InvalidArguments(String),
}

/// Approval requirement returned before tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecApprovalRequirement {
    /// No prompt is required.
    Skip {
        /// Whether sandbox should be bypassed on first attempt.
        bypass_sandbox: bool,
        /// Optional command-prefix amendment for later prompts.
        proposed_execpolicy_amendment: Option<protocol::ExecPolicyAmendment>,
    },
    /// User approval is required.
    NeedsApproval {
        /// Human-readable approval reason.
        reason: Option<String>,
        /// Optional command-prefix amendment.
        proposed_execpolicy_amendment: Option<protocol::ExecPolicyAmendment>,
    },
    /// Execution is forbidden by policy.
    Forbidden {
        /// Model-facing rejection reason.
        reason: String,
    },
}

impl ExecApprovalRequirement {
    /// Return a requirement that allows execution without prompting.
    #[must_use]
    pub fn skip() -> Self {
        Self::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        }
    }

    /// Return a requirement that prompts without an amendment.
    #[must_use]
    pub fn approval_required() -> Self {
        Self::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{
        AgentPath, ApprovalMode, AskForApproval, SessionId, ToolContext,
    };
    use std::path::PathBuf;

    /// Verifies generic invocations preserve raw arguments for fallback tools.
    #[test]
    fn generic_invocation_preserves_raw_arguments() {
        let invocation = ToolInvocation::generic(
            "call-1",
            "shell",
            serde_json::json!({ "command": "pwd" }),
            &ToolContext::builder()
                .session_id(SessionId::from("s1"))
                .cwd(PathBuf::from("/tmp"))
                .agent_path(AgentPath::root())
                .approval_mode(ApprovalMode::RequestApproval)
                .approval_policy(AskForApproval::OnRequest)
                .build(),
        );

        assert_eq!(invocation.call_id, "call-1");
        assert_eq!(invocation.tool_name, "shell");
        assert_eq!(invocation.raw_arguments["command"], "pwd");
    }
}
