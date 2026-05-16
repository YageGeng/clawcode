//! Streaming event types emitted from the kernel to the frontend.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::{AgentPath, AgentStatus};
use crate::item::{TurnId, TurnItem};
use crate::permission::PermissionRequest;
use crate::plan::PlanEntry;
use crate::session::SessionId;
use crate::tool::ToolCallStatus;

/// The content of a streamed tool-call delta.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToolCallDeltaContent {
    /// Tool/function name delivered by the provider.
    Name(String),
    /// Partial JSON argument data delivered by the provider.
    Delta(String),
}

impl ToolCallDeltaContent {
    /// Create a `Name` content from a string.
    #[inline(always)]
    pub fn name(name: impl Into<String>) -> Self {
        ToolCallDeltaContent::Name(name.into())
    }

    /// Create a `Delta` content from a string.
    #[inline(always)]
    pub fn delta(delta: impl Into<String>) -> Self {
        ToolCallDeltaContent::Delta(delta.into())
    }
}

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
    /// Incremental tool call parameter streaming from the LLM.
    ToolCallDelta {
        session_id: SessionId,
        /// Matches the upcoming ToolCall event id.
        call_id: String,
        /// Tool name or arguments fragment.
        content: ToolCallDeltaContent,
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
    /// A structured turn item started.
    ItemStarted {
        session_id: SessionId,
        /// Turn that owns the item.
        turn_id: TurnId,
        /// Structured item payload for display.
        item: TurnItem,
    },
    /// A structured turn item completed.
    ItemCompleted {
        session_id: SessionId,
        /// Turn that owns the item.
        turn_id: TurnId,
        /// Structured item payload for display.
        item: TurnItem,
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
    /// The kernel requests user approval before executing a tool.
    ExecApprovalRequested {
        session_id: SessionId,
        /// Identifies the tool call awaiting approval.
        call_id: String,
        /// Name of the tool being requested.
        tool_name: String,
        /// JSON arguments for the tool invocation.
        arguments: serde_json::Value,
        /// Working directory for the tool execution.
        cwd: PathBuf,
    },
    /// A sub-agent's runtime status changed.
    AgentStatusChange {
        session_id: SessionId,
        /// The agent whose status changed.
        agent_path: AgentPath,
        /// New status.
        status: AgentStatus,
    },
    /// A sub-agent was spawned.
    AgentSpawned {
        session_id: SessionId,
        /// Canonical path of the new agent.
        agent_path: AgentPath,
        /// Human-readable nickname.
        agent_nickname: String,
        /// Role assigned at spawn.
        agent_role: String,
    },
    /// The current turn has completed.
    TurnComplete {
        session_id: SessionId,
        /// Reason the turn stopped.
        stop_reason: StopReason,
    },
}

impl Event {
    /// Create an `AgentMessageChunk` event.
    #[inline(always)]
    pub fn message_chunk(session_id: impl Into<SessionId>, text: impl Into<String>) -> Self {
        Event::AgentMessageChunk {
            session_id: session_id.into(),
            text: text.into(),
        }
    }

    /// Create an `AgentThoughtChunk` event for reasoning / thinking deltas.
    #[inline(always)]
    pub fn thought_chunk(session_id: impl Into<SessionId>, text: impl Into<String>) -> Self {
        Event::AgentThoughtChunk {
            session_id: session_id.into(),
            text: text.into(),
        }
    }

    /// Create a `ToolCall` event.
    #[inline(always)]
    pub fn tool_call(
        session_id: impl Into<SessionId>,
        agent_path: impl Into<AgentPath>,
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<serde_json::Value>,
        status: ToolCallStatus,
    ) -> Self {
        Event::ToolCall {
            session_id: session_id.into(),
            agent_path: agent_path.into(),
            call_id: call_id.into(),
            name: name.into(),
            arguments: arguments.into(),
            status,
        }
    }

    /// Create a `ToolCallDelta` event for streaming tool call parameter updates.
    #[inline(always)]
    pub fn tool_call_delta(
        session_id: impl Into<SessionId>,
        call_id: impl Into<String>,
        content: ToolCallDeltaContent,
    ) -> Self {
        Event::ToolCallDelta {
            session_id: session_id.into(),
            call_id: call_id.into(),
            content,
        }
    }

    /// Create a `ToolCallUpdate` event for incremental tool output or status changes.
    #[inline(always)]
    pub fn tool_call_update(
        session_id: impl Into<SessionId>,
        call_id: impl Into<String>,
        output_delta: Option<String>,
        status: Option<ToolCallStatus>,
    ) -> Self {
        Event::ToolCallUpdate {
            session_id: session_id.into(),
            call_id: call_id.into(),
            output_delta,
            status,
        }
    }

    /// Create an `ItemStarted` event for a structured turn item.
    #[inline(always)]
    pub fn item_started(
        session_id: impl Into<SessionId>,
        turn_id: impl Into<TurnId>,
        item: TurnItem,
    ) -> Self {
        Event::ItemStarted {
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            item,
        }
    }

    /// Create an `ItemCompleted` event for a structured turn item.
    #[inline(always)]
    pub fn item_completed(
        session_id: impl Into<SessionId>,
        turn_id: impl Into<TurnId>,
        item: TurnItem,
    ) -> Self {
        Event::ItemCompleted {
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            item,
        }
    }

    /// Create a `UsageUpdate` event with token consumption info.
    #[inline(always)]
    pub fn usage_update(
        session_id: impl Into<SessionId>,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Self {
        Event::UsageUpdate {
            session_id: session_id.into(),
            input_tokens,
            output_tokens,
        }
    }

    /// Create a `TurnComplete` event.
    #[inline(always)]
    pub fn turn_complete(session_id: impl Into<SessionId>, stop_reason: StopReason) -> Self {
        Event::TurnComplete {
            session_id: session_id.into(),
            stop_reason,
        }
    }

    /// Create an `ExecApprovalRequested` event to request user approval before tool execution.
    #[inline(always)]
    pub fn exec_approval(
        session_id: impl Into<SessionId>,
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        arguments: impl Into<serde_json::Value>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Event::ExecApprovalRequested {
            session_id: session_id.into(),
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            arguments: arguments.into(),
            cwd: cwd.into(),
        }
    }

    /// Create a `PlanUpdate` event.
    #[inline(always)]
    pub fn plan_update(session_id: impl Into<SessionId>, entries: Vec<PlanEntry>) -> Self {
        Event::PlanUpdate {
            session_id: session_id.into(),
            entries,
        }
    }

    /// Create a `PermissionRequested` event.
    #[inline(always)]
    pub fn permission_requested(
        session_id: impl Into<SessionId>,
        request: PermissionRequest,
    ) -> Self {
        Event::PermissionRequested {
            session_id: session_id.into(),
            request,
        }
    }

    /// Create an `AgentStatusChange` event for sub-agent lifecycle tracking.
    #[inline(always)]
    pub fn agent_status_change(
        session_id: impl Into<SessionId>,
        agent_path: impl Into<AgentPath>,
        status: AgentStatus,
    ) -> Self {
        Event::AgentStatusChange {
            session_id: session_id.into(),
            agent_path: agent_path.into(),
            status,
        }
    }

    /// Create an `AgentSpawned` event.
    #[inline(always)]
    pub fn agent_spawned(
        session_id: impl Into<SessionId>,
        agent_path: impl Into<AgentPath>,
        agent_nickname: impl Into<String>,
        agent_role: impl Into<String>,
    ) -> Self {
        Event::AgentSpawned {
            session_id: session_id.into(),
            agent_path: agent_path.into(),
            agent_nickname: agent_nickname.into(),
            agent_role: agent_role.into(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{FileChangeItem, FileChangeStatus};

    /// Verifies that item lifecycle events preserve the owning turn id.
    #[test]
    fn item_lifecycle_events_roundtrip_turn_id() {
        let session_id = SessionId("session-1".to_string());
        let turn_id = TurnId("turn-1".to_string());
        let item = TurnItem::FileChange(
            FileChangeItem::builder()
                .id("call-1".to_string())
                .title("Apply patch".to_string())
                .changes(Vec::new())
                .status(FileChangeStatus::InProgress)
                .build(),
        );

        let started = Event::item_started(session_id.clone(), turn_id.clone(), item.clone());
        let completed = Event::item_completed(session_id, turn_id.clone(), item);

        let started_json = serde_json::to_string(&started).expect("serialize started event");
        let completed_json = serde_json::to_string(&completed).expect("serialize completed event");
        let decoded_started: Event =
            serde_json::from_str(&started_json).expect("deserialize started event");
        let decoded_completed: Event =
            serde_json::from_str(&completed_json).expect("deserialize completed event");

        assert!(matches!(
            decoded_started,
            Event::ItemStarted { turn_id: decoded, .. } if decoded == turn_id
        ));
        assert!(matches!(
            decoded_completed,
            Event::ItemCompleted { turn_id: decoded, .. } if decoded == turn_id
        ));
    }
}
