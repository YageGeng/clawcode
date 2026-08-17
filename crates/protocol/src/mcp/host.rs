use serde::{Deserialize, Serialize};

use crate::{
    AgentMessage, ContentBlock, McpAuthorizationRequest,
    McpAuthorizationResult, McpServerId, Role, SessionId, StopReason,
    TimestampMs, TraceId, TurnId,
};

/// Correlation shared by every Turn-scoped MCP Host request.
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
pub struct McpRequestContext {
    /// Server that initiated the Host request.
    pub server_id: McpServerId,
    /// Session that owns the independent Server connection.
    pub session_id: SessionId,
    /// Turn during which the Host request occurred.
    pub turn_id: TurnId,
    /// Trace inherited from the application through Kernel and Provider.
    pub trace_id: TraceId,
    /// Unix millisecond time when the Host received the request.
    pub requested_at_ms: TimestampMs,
}

/// Provider-neutral Sampling request initiated by an MCP Server.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpSamplingRequest {
    /// Turn and trace correlation.
    pub context: McpRequestContext,
    /// Ordered messages supplied by the MCP Server.
    #[serde(default)]
    #[builder(default)]
    pub messages: Vec<AgentMessage>,
    /// Optional Sampling-specific system instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub system_prompt: Option<String>,
    /// Maximum model output tokens allowed for this request.
    pub max_tokens: u64,
    /// Optional model temperature requested by the Server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub temperature: Option<f64>,
}

/// Provider-neutral result returned to an MCP Sampling request.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct McpSamplingResult {
    /// Role assigned to the sampled message.
    pub role: Role,
    /// Ordered sampled content blocks.
    pub blocks: Vec<ContentBlock>,
    /// Kernel-normalized reason Sampling stopped.
    pub stop_reason: StopReason,
    /// Provider and model identifier used for Sampling.
    pub model: String,
}

/// User interaction requested by a Legacy elicitation or Modern MRTR round.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum McpElicitationMode {
    /// Structured form interaction validated against the supplied JSON Schema.
    Form {
        /// Human-readable interaction prompt.
        message: String,
        /// Server-provided object schema.
        requested_schema: serde_json::Value,
    },
    /// Browser interaction completed through a Server-provided URL.
    Url {
        /// Human-readable interaction prompt.
        message: String,
        /// Validated URL the user must open.
        url: String,
        /// Opaque Server elicitation identifier.
        elicitation_id: String,
    },
}

/// Turn-scoped request for structured user input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpElicitationRequest {
    /// Stable Session-unique identifier used by ACP to route the user response.
    pub request_id: String,
    /// Turn and trace correlation.
    pub context: McpRequestContext,
    /// Form or URL interaction requested by the Server.
    pub mode: McpElicitationMode,
}

/// Complete transient elicitation state for one active Session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpElicitationSnapshot {
    /// Session that owns every pending request in this snapshot.
    pub session_id: SessionId,
    /// Pending requests ordered by their stable request identifier.
    pub requests: Vec<McpElicitationRequest>,
}

/// User disposition returned for an MCP elicitation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpElicitationAction {
    /// The user accepted and supplied content when required.
    Accept,
    /// The user explicitly declined the request.
    Decline,
    /// The user or owning Turn cancelled the request.
    Cancel,
}

/// Typed user response returned to an MCP elicitation request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpElicitationResult {
    /// User disposition.
    pub action: McpElicitationAction,
    /// Structured accepted content when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
}

/// ACP request that resolves one pending Turn-scoped MCP elicitation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpElicitationResponseRequest {
    /// Session that owns the active Turn.
    pub session_id: SessionId,
    /// Session-unique request identity emitted with the elicitation event.
    pub request_id: String,
    /// User disposition and optional structured content.
    pub result: McpElicitationResult,
}

/// Turn-scoped request for filesystem roots allowed by the Kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRootsRequest {
    /// Turn and trace correlation.
    pub context: McpRequestContext,
}

/// One application-approved root exposed without ACP Client filesystem callbacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRoot {
    /// Absolute file URI allowed for this Session.
    pub uri: String,
    /// Optional user-facing root name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Complete root list returned to one MCP Server request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRootsResult {
    /// Ordered application-approved roots.
    pub roots: Vec<McpRoot>,
}

/// Protocol-neutral request dispatched from MCP into its owning Host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "request", rename_all = "camelCase")]
pub enum McpHostRequest {
    /// Model Sampling request.
    Sampling(McpSamplingRequest),
    /// User elicitation request.
    Elicitation(McpElicitationRequest),
    /// Application root request.
    Roots(McpRootsRequest),
    /// OAuth authorization continuation request.
    Authorization(McpAuthorizationRequest),
}

/// Protocol-neutral response returned by an MCP Host implementation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "response", rename_all = "camelCase")]
pub enum McpHostResponse {
    /// Model Sampling response.
    Sampling(McpSamplingResult),
    /// User elicitation response.
    Elicitation(McpElicitationResult),
    /// Application root response.
    Roots(McpRootsResult),
    /// OAuth authorization continuation response.
    Authorization(McpAuthorizationResult),
}
