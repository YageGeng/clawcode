use chrono::{DateTime, Utc};

use crate::events::ToolCallInFlightState;

use super::registry::InFlightToolCallSnapshot;

/// Public handle-level runtime snapshot that summarizes tool-call progress within a turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallRuntimeSnapshot {
    pub entries: Vec<ToolCallRuntimeEntry>,
    pub queued_handles: Vec<String>,
    pub running_handles: Vec<String>,
    pub completed_handles: Vec<String>,
    pub cancelled_handles: Vec<String>,
    pub failed_handles: Vec<String>,
}

impl ToolCallRuntimeSnapshot {
    /// Merges one iteration snapshot into the turn-level runtime snapshot returned to callers.
    pub(crate) fn extend(&mut self, other: Self) {
        self.entries.extend(other.entries);
        self.queued_handles.extend(other.queued_handles);
        self.running_handles.extend(other.running_handles);
        self.completed_handles.extend(other.completed_handles);
        self.cancelled_handles.extend(other.cancelled_handles);
        self.failed_handles.extend(other.failed_handles);
    }
}

/// Public per-handle runtime record describing one tool call in the current turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRuntimeEntry {
    pub handle_id: String,
    pub name: String,
    pub tool_id: String,
    pub tool_call_id: String,
    pub state: ToolCallInFlightState,
    pub output_summary: Option<String>,
    pub structured_output: Option<serde_json::Value>,
    pub error_summary: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
}

/// Converts the internal registry snapshot into the public runtime return shape.
impl From<InFlightToolCallSnapshot> for ToolCallRuntimeSnapshot {
    fn from(value: InFlightToolCallSnapshot) -> Self {
        Self {
            entries: value
                .entries
                .into_iter()
                .map(|entry| ToolCallRuntimeEntry {
                    handle_id: entry.handle_id,
                    name: entry.name,
                    tool_id: entry.tool_id,
                    tool_call_id: entry.tool_call_id,
                    state: entry.state,
                    output_summary: entry.output_summary,
                    structured_output: entry.structured_output,
                    error_summary: entry.error_summary,
                    started_at: entry.started_at,
                    finished_at: entry.finished_at,
                    duration_ms: entry.duration_ms,
                })
                .collect(),
            queued_handles: value.queued_handles,
            running_handles: value.running_handles,
            completed_handles: value.completed_handles,
            cancelled_handles: value.cancelled_handles,
            failed_handles: value.failed_handles,
        }
    }
}
