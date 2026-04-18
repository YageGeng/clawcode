pub mod executor;
pub mod builtin {
    pub use tools::builtin::apply_patch::ApplyPatchTool;
    pub use tools::builtin::shell::{
        ExecCommandTool, UnifiedExecProcessManager, UnifiedExecRuntime, WriteStdinTool,
    };
}
pub use tools::{
    ApprovalRequirement, RiskLevel, ToolApprovalHandler, ToolApprovalRequest, ToolCallRequest,
    ToolContext, ToolHandler as Tool, ToolInvocation, ToolMetadata, ToolOutput, ToolPayload,
    create, plan, registry, router,
};

/// Re-exports the extracted tools crate for compatibility with existing callers.
pub type ToolResult<T> = tools::Result<T>;

/// Re-exports the extracted tool registry type for compatibility with existing callers.
pub type ToolRegistry = tools::ToolRegistry;

/// Re-exports the extracted tool router type for compatibility with existing callers.
pub type ToolRouter = tools::ToolRouter;
