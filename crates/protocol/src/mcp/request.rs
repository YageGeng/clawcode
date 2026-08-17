use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    ContentBlock, McpPromptRef, McpResourceRef, McpServerId, McpToolRef, Role,
    SessionId,
};

/// Session-scoped request for one explicit exact-protocol Server reconnect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpReconnectRequest {
    /// Agent Session owning the Server runtime.
    pub session_id: SessionId,
    /// Configured Server to stop and bootstrap again.
    pub server_id: McpServerId,
}

/// Session-bound request to retrieve one discovered MCP Prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSessionPromptRequest {
    /// Agent Session owning the Prompt catalog.
    pub session_id: SessionId,
    /// Structured remote Prompt request.
    pub request: McpPromptRequest,
}

/// Session-bound request to read one discovered MCP Resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSessionResourceRequest {
    /// Agent Session owning the Resource catalog.
    pub session_id: SessionId,
    /// Structured remote Resource request.
    pub request: McpResourceRequest,
}

/// Session-bound request to complete one Prompt or Resource Template argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSessionCompletionRequest {
    /// Agent Session owning the Completion target.
    pub session_id: SessionId,
    /// Structured remote Completion request.
    pub request: McpCompletionRequest,
}

/// Routed request to invoke one remote MCP Tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolRequest {
    /// Structured Tool reference retaining its owning Server.
    pub reference: McpToolRef,
    /// JSON object arguments validated by the remote Server.
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// Lossless normalized result from one remote MCP Tool invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolResult {
    /// Ordered displayable MCP content blocks.
    pub content: Vec<ContentBlock>,
    /// Optional structured content retained independently from display blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    /// Whether the remote Server marked the result as failed.
    pub is_error: bool,
}

/// Routed request to render one remote MCP Prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptRequest {
    /// Structured Prompt reference retaining its owning Server.
    pub reference: McpPromptRef,
    /// Named Prompt arguments.
    #[serde(default)]
    pub arguments: BTreeMap<String, String>,
}

/// One role-bearing message returned by an MCP Prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptMessage {
    /// Model-facing role declared by the remote Prompt.
    pub role: Role,
    /// Lossless Prompt content.
    pub content: ContentBlock,
}

/// Complete normalized MCP Prompt result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptResult {
    /// Optional Server-provided result description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Ordered Prompt messages.
    pub messages: Vec<McpPromptMessage>,
}

/// Routed request to read one remote MCP Resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceRequest {
    /// Structured Resource reference retaining its owning Server.
    pub reference: McpResourceRef,
}

/// Complete normalized MCP Resource read result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceResult {
    /// Ordered embedded Resource blocks.
    pub contents: Vec<ContentBlock>,
}

/// Typed catalog entity targeted by an MCP Completion request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "reference", rename_all = "camelCase")]
pub enum McpCompletionTarget {
    /// Completion for one Prompt argument.
    Prompt(McpPromptRef),
    /// Completion for one Resource Template argument.
    ResourceTemplate {
        /// Server that owns the Resource Template.
        server_id: McpServerId,
        /// Original URI template.
        uri_template: String,
    },
}

impl McpCompletionTarget {
    /// Returns the Server owning this structured Completion target.
    #[must_use]
    pub fn server_id(&self) -> &McpServerId {
        match self {
            Self::Prompt(reference) => &reference.server_id,
            Self::ResourceTemplate { server_id, .. } => server_id,
        }
    }
}

impl From<McpPromptRef> for McpCompletionTarget {
    /// Wraps a structured Prompt reference as a Completion target.
    fn from(reference: McpPromptRef) -> Self {
        Self::Prompt(reference)
    }
}

/// Typed MCP Completion request routed through one Session catalog.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpCompletionRequest {
    /// Prompt or Resource Template receiving Completion.
    pub target: McpCompletionTarget,
    /// Argument being completed.
    pub argument_name: String,
    /// Partial user-authored argument value.
    pub argument_value: String,
    /// Deterministically ordered values of other arguments.
    #[serde(default)]
    #[builder(default)]
    pub context: BTreeMap<String, String>,
}

/// Normalized Completion candidates returned by an MCP Server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCompletionResult {
    /// Ordered completion candidate strings.
    pub values: Vec<String>,
    /// Total candidate count when the Server reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Whether additional candidates remain available.
    pub has_more: bool,
}
