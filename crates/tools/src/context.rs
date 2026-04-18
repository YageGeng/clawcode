use std::{fmt, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use snafu::ResultExt;

use crate::{Result, error::JsonSnafu};

/// Describes the risk profile of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// Declares whether a tool must be approved before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    Never,
    Always,
}

/// Carries approval-time information for one tool invocation.
#[derive(Debug, Clone)]
pub struct ToolApprovalRequest {
    pub tool: String,
    pub call_id: Option<String>,
    pub session_id: String,
    pub thread_id: String,
    pub arguments: serde_json::Value,
}

/// Evaluates whether a tool call is approved at runtime.
pub type ToolApprovalHandler = Arc<dyn Fn(&ToolApprovalRequest) -> bool + Send + Sync>;

/// Stores runtime metadata that affects dispatch behavior.
#[derive(Debug, Clone)]
pub struct ToolMetadata {
    pub risk_level: RiskLevel,
    pub approval: ApprovalRequirement,
    pub timeout: Duration,
}

impl Default for ToolMetadata {
    /// Builds the default low-risk, no-approval metadata.
    fn default() -> Self {
        Self {
            risk_level: RiskLevel::Low,
            approval: ApprovalRequirement::Never,
            timeout: Duration::from_secs(10),
        }
    }
}

/// Carries stable per-call execution context into handlers.
#[derive(Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub thread_id: String,
    pub enforce_tool_approvals: bool,
    pub tool_approval_handler: Option<ToolApprovalHandler>,
}

impl fmt::Debug for ToolContext {
    /// Formats the context without trying to print the approval callback.
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
    /// Builds a tool context from runtime identifiers.
    pub fn new(session_id: impl ToString, thread_id: impl ToString) -> Self {
        Self {
            session_id: session_id.to_string(),
            thread_id: thread_id.to_string(),
            enforce_tool_approvals: false,
            tool_approval_handler: None,
        }
    }

    /// Enables or disables mandatory approval enforcement for this call path.
    pub fn with_tool_approval_enforcement(mut self, enforce_tool_approvals: bool) -> Self {
        self.enforce_tool_approvals = enforce_tool_approvals;
        self
    }

    /// Installs an approval callback for handlers that require confirmation.
    pub fn with_tool_approval_handler(
        mut self,
        handler: impl Fn(&ToolApprovalRequest) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.tool_approval_handler = Some(Arc::new(handler));
        self
    }

    /// Installs an optional approval callback without allocating a no-op closure.
    pub fn with_tool_approval_handler_if_needed(
        mut self,
        handler: Option<ToolApprovalHandler>,
    ) -> Self {
        self.tool_approval_handler = handler;
        self
    }
}

/// Stores both the plain-text and structured response of a tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub structured: serde_json::Value,
}

impl ToolOutput {
    /// Builds a text-first tool output while preserving a simple structured body.
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            text: text.clone(),
            structured: serde_json::json!({ "text": text }),
        }
    }
}

/// Represents the executable payload for one tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToolPayload {
    Function { arguments: serde_json::Value },
}

impl ToolPayload {
    /// Returns the function arguments when this invocation came from a function tool call.
    pub fn function_arguments(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Function { arguments } => Some(arguments),
        }
    }
}

/// Describes one tool call emitted by the model adapter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallRequest {
    pub id: String,
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ToolCallRequest {
    /// Builds a tool call request with a generated internal identifier.
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

/// Carries the normalized payload plus runtime context for one executable tool call.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub id: String,
    pub call_id: Option<String>,
    pub tool_name: String,
    pub payload: ToolPayload,
    pub context: ToolContext,
}

impl ToolInvocation {
    /// Builds an invocation from a model-emitted tool call request.
    pub fn from_call_request(call: ToolCallRequest, context: ToolContext) -> Self {
        Self {
            id: call.id,
            call_id: call.call_id,
            tool_name: call.name,
            payload: ToolPayload::Function {
                arguments: call.arguments,
            },
            context,
        }
    }

    /// Returns the normalized call id used by downstream events and tool results.
    pub fn effective_call_id(&self) -> String {
        self.call_id.clone().unwrap_or_else(|| self.id.clone())
    }

    /// Returns the function arguments for function-style tool invocations.
    pub fn function_arguments(&self) -> Option<&serde_json::Value> {
        self.payload.function_arguments()
    }

    /// Deserializes function arguments into a typed structure for handler execution.
    pub fn parse_function_arguments<T>(&self, stage: &'static str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let arguments = self
            .function_arguments()
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        serde_json::from_value(arguments).context(JsonSnafu {
            stage: stage.to_string(),
        })
    }
}
