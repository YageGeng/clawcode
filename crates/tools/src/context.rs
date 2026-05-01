use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use snafu::ResultExt;

use crate::{
    Result,
    collaboration::{AgentRuntimeContext, CollaborationRuntimeHandle},
    error::JsonSnafu,
};

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

/// Runtime approval behavior selected for a tool dispatch context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolApprovalProfile {
    /// Allows every tool call without consulting an approval handler.
    TrustAll,
    /// Asks for approval before every tool call.
    AskAlways,
    /// Follows each tool's static `ToolMetadata::approval` requirement.
    Default,
}

impl ToolApprovalProfile {
    /// Returns whether this profile requires approval for a tool requirement.
    pub fn requires_approval(self, requirement: ApprovalRequirement) -> bool {
        match self {
            Self::TrustAll => false,
            Self::AskAlways => true,
            Self::Default => match requirement {
                ApprovalRequirement::Never => false,
                ApprovalRequirement::Always => true,
            },
        }
    }
}

impl Default for ToolApprovalProfile {
    /// Builds the low-friction default for direct tool dispatch contexts.
    fn default() -> Self {
        Self::TrustAll
    }
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

/// Future returned by asynchronous tool approval handlers.
pub type ToolApprovalFuture = Pin<Box<dyn Future<Output = bool> + Send>>;

/// Approves or rejects a high-risk tool call at dispatch time.
pub trait ToolApproval: Send + Sync {
    /// Returns whether this tool call is approved for execution.
    fn approve(&self, request: ToolApprovalRequest) -> ToolApprovalFuture;
}

impl<F> ToolApproval for F
where
    F: Fn(ToolApprovalRequest) -> ToolApprovalFuture + Send + Sync,
{
    /// Delegates approval to closure-backed handlers.
    fn approve(&self, request: ToolApprovalRequest) -> ToolApprovalFuture {
        self(request)
    }
}

/// Evaluates whether a tool call is approved at runtime.
pub type ToolApprovalHandler = Arc<dyn ToolApproval>;

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
    pub agent: AgentRuntimeContext,
    pub approval_profile: ToolApprovalProfile,
    pub tool_approval_handler: Option<ToolApprovalHandler>,
    pub collaboration_runtime: Option<CollaborationRuntimeHandle>,
}

impl fmt::Debug for ToolContext {
    /// Formats the context without trying to print the approval callback.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolContext")
            .field("session_id", &self.session_id)
            .field("thread_id", &self.thread_id)
            .field("agent", &self.agent)
            .field("approval_profile", &self.approval_profile)
            .field(
                "tool_approval_handler",
                &self.tool_approval_handler.as_ref().map(|_| "<function>"),
            )
            .field(
                "collaboration_runtime",
                &self.collaboration_runtime.as_ref().map(|_| "<runtime>"),
            )
            .finish()
    }
}

impl ToolContext {
    /// Builds a tool context from runtime identifiers.
    pub fn new(session_id: impl Into<String>, thread_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            thread_id: thread_id.into(),
            agent: AgentRuntimeContext::default(),
            approval_profile: ToolApprovalProfile::TrustAll,
            tool_approval_handler: None,
            collaboration_runtime: None,
        }
    }

    /// Attaches the active agent identity and stable environment to this tool call.
    pub fn with_agent_runtime_context(mut self, agent: AgentRuntimeContext) -> Self {
        self.agent = agent;
        self
    }

    /// Selects the runtime approval profile for this call path.
    pub fn with_tool_approval_profile(mut self, approval_profile: ToolApprovalProfile) -> Self {
        self.approval_profile = approval_profile;
        self
    }

    /// Installs an approval callback for handlers that require confirmation.
    pub fn with_tool_approval_handler(
        mut self,
        handler: impl Fn(ToolApprovalRequest) -> ToolApprovalFuture + Send + Sync + 'static,
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

    /// Installs a collaboration runtime for mailbox-driven subagent tools.
    pub fn with_collaboration_runtime(mut self, runtime: CollaborationRuntimeHandle) -> Self {
        self.collaboration_runtime = Some(runtime);
        self
    }

    /// Installs an optional collaboration runtime without allocating a no-op wrapper.
    pub fn with_collaboration_runtime_if_needed(
        mut self,
        runtime: Option<CollaborationRuntimeHandle>,
    ) -> Self {
        self.collaboration_runtime = runtime;
        self
    }
}

/// Carries the failure message exposed by tool outputs that represent execution errors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutputError {
    pub message: String,
}

/// Describes the built-in `fs/read_text_file` structured payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadTextFileStructuredOutput {
    pub content: String,
}

/// Describes the built-in `fs/write_text_file` structured payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteTextFileStructuredOutput {
    pub ok: bool,
}

/// Describes the built-in shell tool structured payload shared by one-shot and session modes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellStructuredOutput {
    pub running: bool,
    pub session_id: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// Describes one file entry in the `apply_patch` metadata payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPatchFileMetadata {
    pub file_path: String,
    pub relative_path: String,
    pub r#type: String,
    pub patch: String,
    pub additions: usize,
    pub deletions: usize,
    pub move_path: Option<String>,
}

/// Describes one diagnostic entry attached to an `apply_patch` result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDiagnostic {
    pub message: String,
    pub severity: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// Describes the metadata block attached to the `apply_patch` structured payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchMetadata {
    pub diff: String,
    pub files: Vec<ApplyPatchFileMetadata>,
    pub diagnostics: BTreeMap<String, Vec<ToolDiagnostic>>,
}

/// Describes the built-in `apply_patch` structured payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchStructuredOutput {
    pub title: String,
    pub output: String,
    pub metadata: ApplyPatchMetadata,
}

/// Stores the strong-typed structured response of a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuredToolOutput {
    Text {
        text: String,
    },
    Failure {
        success: bool,
        error: ToolOutputError,
    },
    ReadTextFile(ReadTextFileStructuredOutput),
    WriteTextFile(WriteTextFileStructuredOutput),
    Shell(ShellStructuredOutput),
    ApplyPatch(ApplyPatchStructuredOutput),
    /// Preserves arbitrary tool-specific JSON payloads without inventing a parallel JSON type.
    Json(serde_json::Value),
}

impl StructuredToolOutput {
    /// Builds the default text payload shape used by text-first tool outputs.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Builds the default failure payload shape used by failed tool outputs.
    pub fn failure(message: impl Into<String>) -> Self {
        Self::Failure {
            success: false,
            error: ToolOutputError {
                message: message.into(),
            },
        }
    }

    /// Converts a `serde_json::Value` into the generic structured JSON variant.
    pub fn json_value(value: serde_json::Value) -> Self {
        Self::Json(value)
    }

    /// Returns whether this structured payload is exactly the default text shape for `text`.
    pub fn is_plain_text_equivalent(&self, text: &str) -> bool {
        matches!(self, Self::Text { text: structured_text } if structured_text == text)
    }

    /// Serializes the structured payload into the JSON value expected by wire formats.
    pub fn to_serde_value(&self) -> serde_json::Value {
        match self {
            Self::Text { text } => serde_json::json!({ "text": text }),
            Self::Failure { success, error } => serde_json::json!({
                "success": success,
                "error": error,
            }),
            Self::ReadTextFile(output) => {
                serde_json::to_value(output).unwrap_or(serde_json::Value::Null)
            }
            Self::WriteTextFile(output) => {
                serde_json::to_value(output).unwrap_or(serde_json::Value::Null)
            }
            Self::Shell(output) => serde_json::to_value(output).unwrap_or(serde_json::Value::Null),
            Self::ApplyPatch(output) => {
                serde_json::to_value(output).unwrap_or(serde_json::Value::Null)
            }
            Self::Json(value) => value.clone(),
        }
    }
}

/// Stores both the plain-text and structured response of a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub text: String,
    pub structured: StructuredToolOutput,
}

impl ToolOutput {
    /// Builds a text-first tool output while preserving a simple structured body.
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            text: text.clone(),
            structured: StructuredToolOutput::text(text),
        }
    }

    /// Builds a failure-style output while preserving error details for model-visible
    /// tool-result content.
    pub fn failure(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            text: message.clone(),
            structured: StructuredToolOutput::failure(message),
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
