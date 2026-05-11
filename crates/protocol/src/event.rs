//! Streaming event types emitted from the kernel to the frontend.

use serde::{Deserialize, Serialize};

use crate::agent::{AgentPath, AgentStatus};
use crate::permission::PermissionRequest;
use crate::plan::PlanEntry;
use crate::session::SessionId;
use crate::tool::ToolCallStatus;

/// Streaming event emitted from the kernel to the frontend.
///
/// Each event carries a `session_id` and represents a discrete update
/// the frontend should render: text deltas, tool calls, plan changes, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Text delta from the assistant message.
    AgentMessageChunk {
        session_id: SessionId,
        /// Incremental text to append.
        text: String,
    },
    /// Reasoning / thinking delta from the assistant.
    AgentThoughtChunk {
        session_id: SessionId,
        /// Incremental thinking text to append.
        text: String,
    },
    /// A tool call was initiated by the assistant.
    ToolCall {
        session_id: SessionId,
        /// Agent that made the tool call.
        agent_path: AgentPath,
        /// Unique call identifier within the turn.
        call_id: String,
        /// Tool/function name.
        name: String,
        /// JSON-encoded arguments.
        arguments: serde_json::Value,
        /// Current execution status.
        status: ToolCallStatus,
    },
    /// Incremental update to an active tool call.
    ToolCallUpdate {
        session_id: SessionId,
        /// The tool call being updated.
        call_id: String,
        /// New output delta to append.
        output_delta: Option<String>,
        /// Updated status, if changed.
        status: Option<ToolCallStatus>,
    },
    /// The agent's execution plan was created or updated.
    PlanUpdate {
        session_id: SessionId,
        /// Complete list of plan entries (replaces previous plan).
        entries: Vec<PlanEntry>,
    },
    /// Token usage information for the current turn.
    UsageUpdate {
        session_id: SessionId,
        /// Number of input (prompt) tokens consumed.
        input_tokens: u64,
        /// Number of output (completion) tokens produced.
        output_tokens: u64,
    },
    /// The kernel is requesting user permission for a tool execution.
    PermissionRequested {
        session_id: SessionId,
        /// The permission request details.
        request: PermissionRequest,
    },
    /// A sub-agent's runtime status changed.
    AgentStatusChange {
        session_id: SessionId,
        /// The agent whose status changed.
        agent_path: AgentPath,
        /// New status.
        status: AgentStatus,
    },
    /// The current turn has completed.
    TurnComplete {
        session_id: SessionId,
        /// Reason the turn stopped.
        stop_reason: StopReason,
    },
}

/// Reason a turn completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Turn finished normally.
    EndTurn,
    /// Turn was cancelled by the user.
    Cancelled,
    /// Turn terminated due to an error.
    Error,
}
