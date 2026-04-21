use std::collections::VecDeque;

use chrono::{DateTime, Utc};

use crate::{
    Result,
    events::ToolCallInFlightState,
    tools::{ToolCallRequest, executor::ToolExecutionRequest},
};

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

/// Queue of tool calls whose output items have fully completed during the current stream.
#[derive(Debug, Clone, Default)]
pub(crate) struct CompletedToolCallQueue {
    calls: VecDeque<ToolCallRequest>,
}

impl CompletedToolCallQueue {
    /// Registers one completed tool call in the same order the model finished it.
    pub(crate) fn push_completed(&mut self, call: ToolCallRequest) {
        self.calls.push_back(call);
    }

    /// Returns true when the stream has not yet completed any executable tool call items.
    pub(crate) fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Consumes the queue and returns the completed tool calls in their preserved order.
    pub(crate) fn into_calls(self) -> Vec<ToolCallRequest> {
        self.calls.into_iter().collect()
    }
}

/// Registry of tool calls that have finished model emission and now belong to the runtime.
#[derive(Debug, Clone, Default)]
pub(crate) struct InFlightToolCallRegistry {
    pub(crate) entries: VecDeque<InFlightToolCallEntry>,
    next_handle_sequence: usize,
}

impl InFlightToolCallRegistry {
    /// Builds an empty registry that continues handle numbering from the current turn counter.
    pub(crate) fn with_next_handle_sequence(next_handle_sequence: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            next_handle_sequence,
        }
    }

    /// Registers one tool call in the same order the model completed it and assigns a stable handle.
    pub(crate) fn register(
        &mut self,
        name: String,
        tool_id: String,
        tool_call_id: String,
        state: ToolCallInFlightState,
    ) -> InFlightToolCallEntry {
        self.next_handle_sequence += 1;
        let entry = InFlightToolCallEntry {
            name,
            tool_id,
            tool_call_id,
            handle_id: format!("tool_exec_{}", self.next_handle_sequence),
            state,
            output_summary: None,
            structured_output: None,
            error_summary: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
        };
        self.entries.push_back(entry.clone());
        entry
    }

    /// Returns the number of registered tool calls that are ready for execution.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns the next turn-global handle sequence after this iteration's registrations.
    pub(crate) fn next_handle_sequence(&self) -> usize {
        self.next_handle_sequence
    }

    /// Updates one registered tool call state while preserving the original registration order.
    pub(crate) fn update_state(
        &mut self,
        handle_id: &str,
        state: ToolCallInFlightState,
    ) -> Option<InFlightToolCallEntry> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.handle_id == handle_id)?;
        entry.apply_state_transition(state);
        Some(entry.clone())
    }

    /// Stores the latest display-friendly tool output summary for a registered handle.
    pub(crate) fn update_output_summary(
        &mut self,
        handle_id: &str,
        output_summary: Option<String>,
    ) -> Option<InFlightToolCallEntry> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.handle_id == handle_id)?;
        entry.output_summary = output_summary;
        Some(entry.clone())
    }

    /// Stores the latest structured tool output payload for a registered handle.
    pub(crate) fn update_structured_output(
        &mut self,
        handle_id: &str,
        structured_output: Option<serde_json::Value>,
    ) -> Option<InFlightToolCallEntry> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.handle_id == handle_id)?;
        entry.structured_output = structured_output;
        Some(entry.clone())
    }

    /// Stores the latest display-friendly error summary for a registered handle.
    pub(crate) fn update_error_summary(
        &mut self,
        handle_id: &str,
        error_summary: Option<String>,
    ) -> Option<InFlightToolCallEntry> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.handle_id == handle_id)?;
        entry.error_summary = error_summary;
        Some(entry.clone())
    }

    /// Returns every registered tool call that has not yet reached a terminal state.
    pub(crate) fn active_entries(&self) -> Vec<InFlightToolCallEntry> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.state != ToolCallInFlightState::Completed
                    && entry.state != ToolCallInFlightState::Cancelled
                    && entry.state != ToolCallInFlightState::Failed
            })
            .cloned()
            .collect()
    }

    /// Summarizes the current in-flight registry as grouped handle lists per runtime state.
    pub(crate) fn snapshot(&self) -> InFlightToolCallSnapshot {
        let mut snapshot = InFlightToolCallSnapshot::default();
        for entry in &self.entries {
            snapshot.entries.push(entry.clone());
            match entry.state {
                ToolCallInFlightState::Queued => {
                    snapshot.queued_handles.push(entry.handle_id.clone());
                }
                ToolCallInFlightState::Running => {
                    snapshot.running_handles.push(entry.handle_id.clone());
                }
                ToolCallInFlightState::Completed => {
                    snapshot.completed_handles.push(entry.handle_id.clone());
                }
                ToolCallInFlightState::Cancelled => {
                    snapshot.cancelled_handles.push(entry.handle_id.clone());
                }
                ToolCallInFlightState::Failed => {
                    snapshot.failed_handles.push(entry.handle_id.clone());
                }
            }
        }
        snapshot
    }

    /// Converts completed tool calls into executor requests by attaching the registered handles.
    #[allow(clippy::result_large_err)]
    pub(crate) fn execution_requests_for_calls(
        &self,
        calls: Vec<ToolCallRequest>,
    ) -> Result<Vec<ToolExecutionRequest>> {
        let mut requests = Vec::with_capacity(calls.len());
        for call in calls {
            let tool_call_id = call.call_id.clone().unwrap_or_else(|| call.id.clone());
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.tool_id == call.id && entry.tool_call_id == tool_call_id)
                .ok_or_else(|| crate::Error::Runtime {
                    message: format!(
                        "missing in-flight handle for tool `{}` / call `{}`",
                        call.id, tool_call_id
                    ),
                    stage: "agent-loop-map-tool-execution-request".to_string(),
                    inflight_snapshot: Some(self.snapshot().into()),
                })?;
            requests.push(ToolExecutionRequest {
                handle_id: entry.handle_id.clone(),
                call,
            });
        }
        Ok(requests)
    }

    /// Updates one handle state and fails with structured context if the registry is inconsistent.
    #[allow(clippy::result_large_err)]
    pub(crate) fn update_state_checked(
        &mut self,
        handle_id: &str,
        state: ToolCallInFlightState,
    ) -> Result<InFlightToolCallEntry> {
        self.update_state(handle_id, state)
            .ok_or_else(|| crate::Error::Runtime {
                message: format!("missing in-flight entry for handle `{handle_id}`"),
                stage: "agent-loop-update-inflight-state".to_string(),
                inflight_snapshot: Some(self.snapshot().into()),
            })
    }

    /// Updates one handle output summary and fails with structured context if the registry is inconsistent.
    #[allow(clippy::result_large_err)]
    pub(crate) fn update_output_summary_checked(
        &mut self,
        handle_id: &str,
        output_summary: Option<String>,
    ) -> Result<InFlightToolCallEntry> {
        self.update_output_summary(handle_id, output_summary)
            .ok_or_else(|| crate::Error::Runtime {
                message: format!("missing in-flight entry for handle `{handle_id}`"),
                stage: "agent-loop-update-output-summary".to_string(),
                inflight_snapshot: Some(self.snapshot().into()),
            })
    }

    /// Updates one handle structured output and fails with structured context if the registry is inconsistent.
    #[allow(clippy::result_large_err)]
    pub(crate) fn update_structured_output_checked(
        &mut self,
        handle_id: &str,
        structured_output: Option<serde_json::Value>,
    ) -> Result<InFlightToolCallEntry> {
        self.update_structured_output(handle_id, structured_output)
            .ok_or_else(|| crate::Error::Runtime {
                message: format!("missing in-flight entry for handle `{handle_id}`"),
                stage: "agent-loop-update-structured-output".to_string(),
                inflight_snapshot: Some(self.snapshot().into()),
            })
    }

    /// Updates one handle error summary and fails with structured context if the registry is inconsistent.
    #[allow(clippy::result_large_err)]
    pub(crate) fn update_error_summary_checked(
        &mut self,
        handle_id: &str,
        error_summary: Option<String>,
    ) -> Result<InFlightToolCallEntry> {
        self.update_error_summary(handle_id, error_summary)
            .ok_or_else(|| crate::Error::Runtime {
                message: format!("missing in-flight entry for handle `{handle_id}`"),
                stage: "agent-loop-update-error-summary".to_string(),
                inflight_snapshot: Some(self.snapshot().into()),
            })
    }
}

/// Metadata for one tool call that has been promoted into the loop's in-flight registry.
#[derive(Debug, Clone)]
pub(crate) struct InFlightToolCallEntry {
    pub(crate) name: String,
    pub(crate) tool_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) handle_id: String,
    pub(crate) state: ToolCallInFlightState,
    pub(crate) output_summary: Option<String>,
    pub(crate) structured_output: Option<serde_json::Value>,
    pub(crate) error_summary: Option<String>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) finished_at: Option<DateTime<Utc>>,
    pub(crate) duration_ms: Option<i64>,
}

impl InFlightToolCallEntry {
    /// Applies one runtime state transition and records timing metadata when it becomes available.
    fn apply_state_transition(&mut self, state: ToolCallInFlightState) {
        let now = Utc::now();
        if state == ToolCallInFlightState::Running && self.started_at.is_none() {
            self.started_at = Some(now);
        }
        if matches!(
            state,
            ToolCallInFlightState::Completed
                | ToolCallInFlightState::Cancelled
                | ToolCallInFlightState::Failed
        ) {
            if self.started_at.is_none() {
                self.started_at = Some(now);
            }
            if self.finished_at.is_none() {
                self.finished_at = Some(now);
            }
            if let (Some(started_at), Some(finished_at)) = (self.started_at, self.finished_at) {
                self.duration_ms = Some((finished_at - started_at).num_milliseconds());
            }
        }
        self.state = state;
    }

    /// Returns the stable identity tuple used by runtime assertions and future handle maps.
    pub(crate) fn identity(&self) -> (&str, &str, &str) {
        (&self.name, &self.tool_id, &self.tool_call_id)
    }
}

/// Aggregated handle-level runtime view for every tool call registered in the current iteration.
#[derive(Debug, Clone, Default)]
pub(crate) struct InFlightToolCallSnapshot {
    entries: Vec<InFlightToolCallEntry>,
    pub(crate) queued_handles: Vec<String>,
    pub(crate) running_handles: Vec<String>,
    pub(crate) completed_handles: Vec<String>,
    pub(crate) cancelled_handles: Vec<String>,
    pub(crate) failed_handles: Vec<String>,
}

impl From<InFlightToolCallSnapshot> for ToolCallRuntimeSnapshot {
    /// Converts the internal registry snapshot into the public runtime return shape.
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
