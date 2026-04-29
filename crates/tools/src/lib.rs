//! Shared tool specifications, registration, and built-in tool handlers.

pub mod builtin;
pub mod context;
pub mod error;
pub mod handler;
pub mod plan;
pub mod registry;
pub mod router;
pub mod spec;

pub use context::{
    ApplyPatchFileMetadata, ApplyPatchMetadata, ApplyPatchStructuredOutput, ApprovalRequirement,
    ReadTextFileStructuredOutput, RiskLevel, ShellStructuredOutput, StructuredToolOutput,
    ToolApproval, ToolApprovalFuture, ToolApprovalHandler, ToolApprovalProfile,
    ToolApprovalRequest, ToolCallRequest, ToolContext, ToolDiagnostic, ToolInvocation,
    ToolMetadata, ToolOutput, ToolOutputError, ToolPayload, WriteTextFileStructuredOutput,
};
pub use error::{Error, Result};
pub use handler::ToolHandler;
pub use plan::{
    PlannedToolHandler, ToolHandlerKind, ToolRegistryPlan, build_default_tool_registry_plan,
};
pub use registry::{ToolRegistry, ToolRegistryBuilder};
pub use router::ToolRouter;
pub use spec::{ConfiguredToolSpec, ToolPromptMetadata, ToolSpec};
