//! Shared tool specifications, registration, and built-in tool handlers.

pub mod builtin;
pub mod context;
pub mod create;
pub mod error;
pub mod handler;
pub mod plan;
pub mod registry;
pub mod router;
pub mod spec;

pub use context::{
    ApprovalRequirement, RiskLevel, ToolApproval, ToolApprovalFuture, ToolApprovalHandler,
    ToolApprovalProfile, ToolApprovalRequest, ToolCallRequest, ToolContext, ToolInvocation,
    ToolMetadata, ToolOutput, ToolPayload,
};
pub use create::{create_default_tool_router, create_file_tool_router_with_root};
pub use error::{Error, Result};
pub use handler::ToolHandler;
pub use plan::{
    PlannedToolHandler, ToolHandlerKind, ToolRegistryPlan, build_default_tool_registry_plan,
};
pub use registry::{ToolRegistry, ToolRegistryBuilder};
pub use router::ToolRouter;
pub use spec::{ConfiguredToolSpec, ToolPromptMetadata, ToolSpec};
