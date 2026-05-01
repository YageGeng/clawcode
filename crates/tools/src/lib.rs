//! Shared tool specifications, registration, and built-in tool handlers.

pub mod builtin;
pub mod collaboration;
pub mod context;
pub mod error;
pub mod handler;
pub mod plan;
pub mod registry;
pub mod router;
pub mod spec;

pub use collaboration::{
    AgentCommandAck, AgentRuntimeContext, AgentStatus, AgentSummary, CloseAgentRequest,
    CollaborationRuntime, CollaborationRuntimeHandle, ListAgentsRequest, ListAgentsResponse,
    MailboxEvent, MailboxEventKind, SendAgentInputRequest, SpawnAgentRequest, SpawnAgentResponse,
    WaitAgentRequest, WaitAgentResponse,
};
pub use context::{
    ApplyPatchFileMetadata, ApplyPatchMetadata, ApplyPatchStructuredOutput, ApprovalRequirement,
    ReadTextFileStructuredOutput, RiskLevel, ShellStructuredOutput, StructuredToolOutput,
    ToolApproval, ToolApprovalFuture, ToolApprovalHandler, ToolApprovalProfile,
    ToolApprovalRequest, ToolCallRequest, ToolContext, ToolDiagnostic, ToolInvocation,
    ToolMetadata, ToolOutput, ToolOutputError, ToolPayload, WriteTextFileStructuredOutput,
};
pub use error::{Error, Result};
pub use handler::ToolHandler;
pub use plan::{PlannedToolHandler, ToolRegistryPlan, build_default_tool_registry_plan};
pub use registry::{ToolRegistry, ToolRegistryBuilder};
pub use router::ToolRouter;
pub use spec::{ConfiguredToolSpec, ToolPromptMetadata, ToolSpec};

/// Creates a unique temporary directory for integration tests.
///
/// The `prefix` parameter disambiguates call sites across crates (e.g. `"kernel-collaboration"`).
#[doc(hidden)]
pub fn test_temp_root(prefix: &str, label: &str) -> std::path::PathBuf {
    let unique = format!(
        "{prefix}-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&path).expect("temp root should be creatable");
    path
}
