pub mod builtin;
pub mod executor;
pub mod registry;

use std::{fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    session::{SessionId, ThreadId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    Never,
    Always,
}

/// Immutable context for evaluating whether a tool call should be allowed to run.
#[derive(Debug, Clone)]
pub struct ToolApprovalRequest {
    pub tool: String,
    pub call_id: Option<String>,
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub arguments: serde_json::Value,
}

/// A runtime hook that decides whether a specific tool call is authorized.
///
/// Returning `true` means the tool is allowed to execute; returning `false`
/// causes execution to fail with `ToolApprovalRequired`.
pub type ToolApprovalHandler = Arc<dyn Fn(&ToolApprovalRequest) -> bool + Send + Sync>;

#[derive(Debug, Clone)]
pub struct ToolMetadata {
    pub risk_level: RiskLevel,
    pub approval: ApprovalRequirement,
    pub timeout: Duration,
}

impl Default for ToolMetadata {
    fn default() -> Self {
        Self {
            risk_level: RiskLevel::Low,
            approval: ApprovalRequirement::Never,
            timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Clone)]
pub struct ToolContext {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    /// Whether the executor must reject tools that require explicit approval.
    pub enforce_tool_approvals: bool,
    /// Optional callback for interactive/programmable approval decisions.
    pub tool_approval_handler: Option<ToolApprovalHandler>,
}

impl fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolContext")
            .field("session_id", &self.session_id)
            .field("thread_id", &self.thread_id)
            .field("enforce_tool_approvals", &self.enforce_tool_approvals)
            .field(
                "tool_approval_handler",
                &self.tool_approval_handler.as_ref().map(|_| "<function>"),
            )
            .finish()
    }
}

impl ToolContext {
    /// Builds the per-invocation tool context from runtime identifiers.
    pub fn new(session_id: SessionId, thread_id: ThreadId) -> Self {
        Self {
            session_id,
            thread_id,
            enforce_tool_approvals: false,
            tool_approval_handler: None,
        }
    }

    /// Returns a context that also carries an explicit approval-enforcement policy.
    pub fn with_tool_approval_enforcement(mut self, enforce_tool_approvals: bool) -> Self {
        self.enforce_tool_approvals = enforce_tool_approvals;
        self
    }

    /// Adds a callback used when a tool call requires explicit approval.
    pub fn with_tool_approval_handler(
        mut self,
        handler: impl Fn(&ToolApprovalRequest) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.tool_approval_handler = Some(Arc::new(handler));
        self
    }

    /// Applies an optional approval handler without constructing a no-op closure.
    pub fn with_tool_approval_handler_if_needed(
        mut self,
        handler: Option<ToolApprovalHandler>,
    ) -> Self {
        self.tool_approval_handler = handler;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub structured: serde_json::Value,
}

impl ToolOutput {
    /// Builds a text-first output while still preserving a structured payload.
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            text: text.clone(),
            structured: serde_json::json!({ "text": text }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallRequest {
    pub id: String,
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ToolCallRequest {
    /// Builds a tool call description that can be executed by the runtime.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            call_id: None,
            name: name.into(),
            arguments,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn parameters(&self) -> serde_json::Value;

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::default()
    }

    fn definition(&self) -> llm::completion::ToolDefinition {
        llm::completion::ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
    }

    /// Executes one tool invocation using JSON arguments and runtime context.
    async fn execute(&self, args: serde_json::Value, context: ToolContext) -> Result<ToolOutput>;
}
