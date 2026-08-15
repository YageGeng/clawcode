use serde::{Deserialize, Serialize};

use crate::{
    AgentMessage, ModelRequest, TimestampMs, ToolCall, ToolResult, TurnRecord,
};

/// Origin of user input entering the extension chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    /// Interactive WebUI or terminal input.
    Interactive,
    /// ACP or another RPC client.
    Rpc,
    /// Input submitted by an extension action.
    Extension,
}

/// Queue behavior for input received while a run is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputStreamingBehavior {
    /// Inject input before the next model turn.
    Steer,
    /// Run input after current agent work settles.
    FollowUp,
}

/// Raw caller input received before prompt and skill expansion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputEvent {
    /// Current text observed by this handler.
    pub text: String,
    /// Source transport or extension action.
    pub source: InputSource,
    /// Delivery behavior when a run is already active.
    pub streaming_behavior: Option<InputStreamingBehavior>,
}

/// Agent execution is about to start with an assembled system prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeAgentStartEvent {
    /// Expanded user prompt text.
    pub prompt: String,
    /// Current complete system prompt.
    pub system_prompt: String,
    /// Skill names available to the session.
    pub skills: Vec<String>,
}

/// An agent loop started.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct AgentStartEvent;

/// An agent loop ended with its run-local messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEndEvent {
    /// Messages consumed or produced by this run.
    pub messages: Vec<AgentMessage>,
}

/// Agent work is idle with no automatic continuation remaining.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct AgentSettledEvent;

/// One model turn started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStartEvent {
    /// Zero-based turn index within the run.
    pub turn_index: usize,
    /// Turn start time in precision-safe Unix milliseconds.
    pub timestamp_ms: TimestampMs,
}

/// One model turn completed with source-ordered tool results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnEndEvent {
    /// Persistable turn outcome and timing.
    pub turn: TurnRecord,
    /// Tool results in assistant source order.
    pub tool_results: Vec<ToolResult>,
}

/// A complete message entered the streaming lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageStartEvent {
    /// Initial complete message snapshot.
    pub message: AgentMessage,
}

/// Typed assistant content delivered during one streaming message lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageUpdate {
    /// Model-visible assistant text fragment.
    Text {
        /// Provider-delivered text fragment.
        delta: String,
    },
    /// Assistant reasoning or thinking fragment.
    Reasoning {
        /// Provider-delivered reasoning fragment.
        delta: String,
    },
    /// Complete tool call emitted by the provider stream.
    ToolCall {
        /// Shared tool-call payload used by the model and tool runtime.
        call: ToolCall,
    },
}

/// A streaming assistant message received an incremental update.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageUpdateEvent {
    /// Current complete message snapshot.
    pub message: AgentMessage,
    /// Strongly typed content added to the message.
    pub update: MessageUpdate,
}

/// A message completed before final persistence and client projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEndEvent {
    /// Current complete message candidate.
    pub message: AgentMessage,
}

/// Model context has been assembled for one provider call.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextEvent {
    /// Request copy that extensions may replace for this provider call only.
    pub request: ModelRequest,
}
