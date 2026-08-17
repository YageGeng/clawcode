use crate::{AgentMessage, ModelUsage, StopReason, ToolCall, ToolDefinition};

/// Optional generation controls applied only to one model request.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModelRequestOptions {
    /// Maximum provider output tokens requested by the caller.
    pub max_tokens: Option<u64>,
    /// Sampling temperature requested by the caller.
    pub temperature: Option<f64>,
}

/// Complete model input assembled for one pi-compatible Turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    /// Ordered active-branch transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Active tools exposed for this request.
    pub tools: Vec<ToolDefinition>,
    /// Request-local generation controls, empty for ordinary agent Turns.
    pub options: ModelRequestOptions,
}

/// Incremental provider-neutral output consumed by the Turn state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelStreamEvent {
    /// Assistant text fragment.
    TextDelta(String),
    /// Assistant reasoning fragment.
    ReasoningDelta(String),
    /// Fully parsed tool call.
    ToolCall(ToolCall),
    /// Provider stream completed with full terminal metadata.
    Finished(ModelFinal),
}

/// Complete provider-neutral terminal result for one model attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFinal {
    /// Kernel-normalized terminal reason.
    pub stop_reason: StopReason,
    /// Provider-native terminal reason retained for diagnostics.
    pub raw_stop_reason: Option<String>,
    /// Complete provider token accounting.
    pub usage: ModelUsage,
}

/// Whether the same provider request may be attempted again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRetryDisposition {
    /// The failure may be transient.
    Retryable,
    /// The same request should fail immediately.
    NonRetryable,
}

/// Safe structured details for one model request or stream failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFailure {
    /// Human-readable failure summary without credentials or headers.
    pub summary: String,
    /// HTTP status code when one was available.
    pub status: Option<u16>,
    /// Provider request retry classification.
    pub retry_disposition: ModelRetryDisposition,
}
