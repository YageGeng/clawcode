//! Shared MCP domain types kept independent from the rmcp wire implementation.

mod auth;
mod catalog;
mod host;
mod identity;
mod request;
mod status;
mod task;

pub use auth::{
    McpAuthorizationContinueRequest, McpAuthorizationRequest,
    McpAuthorizationResult, McpOAuthState, McpOAuthStatus,
};
pub use catalog::{
    McpArgumentInfo, McpCapabilityCounts, McpCatalog, McpPromptInfo,
    McpResourceInfo, McpResourceTemplateInfo, McpServerCapabilities,
    McpToolInfo,
};
pub use host::{
    McpElicitationAction, McpElicitationMode, McpElicitationRequest,
    McpElicitationResponseRequest, McpElicitationResult,
    McpElicitationSnapshot, McpHostRequest, McpHostResponse, McpRequestContext,
    McpRoot, McpRootsRequest, McpRootsResult, McpSamplingRequest,
    McpSamplingResult,
};
pub use identity::{
    McpIdentityError, McpPromptRef, McpProtocolVersion,
    McpProtocolVersionError, McpResourceRef, McpServerId, McpToolRef,
    McpTransportKind,
};
pub use request::{
    McpCompletionRequest, McpCompletionResult, McpCompletionTarget,
    McpPromptMessage, McpPromptRequest, McpPromptResult, McpReconnectRequest,
    McpResourceRequest, McpResourceResult, McpSessionCompletionRequest,
    McpSessionPromptRequest, McpSessionResourceRequest, McpToolRequest,
    McpToolResult,
};
pub use status::{
    McpCatalogRevisions, McpFailureStage, McpServerFailure,
    McpServerImplementation, McpServerState, McpServerStatus, McpSessionChange,
    McpSessionRevisionNotification, McpSessionSnapshot,
};
pub use task::{McpTaskState, McpTaskStatus};
