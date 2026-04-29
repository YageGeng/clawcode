use std::collections::VecDeque;

use chrono::{DateTime, Utc};

use crate::{
    Result,
    events::ToolCallInFlightState,
    tools::{ToolCallRequest, executor::ToolExecutionRequest},
};

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
        // Identifiers must be non-empty — enforced here once rather than on the hot path.
        debug_assert!(
            !matches!(entry.identity(), ("", _, _) | (_, "", _) | (_, _, "")),
            "registry entries must have non-empty identifiers"
        );
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
        structured_output: Option<tools::StructuredToolOutput>,
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

    /// Returns handle IDs for every entry currently in the `Running` state.
    pub(crate) fn running_handle_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.state == ToolCallInFlightState::Running)
            .map(|entry| entry.handle_id.clone())
            .collect()
    }

    /// Returns `(name, handle_id)` pairs for entries whose handle matches any in the given list.
    ///
    /// Only clones the two fields needed for event publishing, avoiding a full entry clone.
    pub(crate) fn names_for_handles(&self, handle_ids: &[String]) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter(|entry| handle_ids.iter().any(|id| id == &entry.handle_id))
            .map(|entry| (entry.name.clone(), entry.handle_id.clone()))
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
                    inflight_snapshot: Some(Box::new(self.snapshot().into())),
                })?;
            requests.push(ToolExecutionRequest {
                handle_id: entry.handle_id.clone(),
                call,
            });
        }
        Ok(requests)
    }

    /// Updates one handle state and fails with structured context if the registry is inconsistent.
    pub(crate) fn update_state_checked(
        &mut self,
        handle_id: &str,
        state: ToolCallInFlightState,
    ) -> Result<InFlightToolCallEntry> {
        self.update_state(handle_id, state)
            .ok_or_else(|| crate::Error::Runtime {
                message: format!("missing in-flight entry for handle `{handle_id}`"),
                stage: "agent-loop-update-inflight-state".to_string(),
                inflight_snapshot: Some(Box::new(self.snapshot().into())),
            })
    }

    /// Updates one handle output summary and fails with structured context if the registry is inconsistent.
    pub(crate) fn update_output_summary_checked(
        &mut self,
        handle_id: &str,
        output_summary: Option<String>,
    ) -> Result<InFlightToolCallEntry> {
        self.update_output_summary(handle_id, output_summary)
            .ok_or_else(|| crate::Error::Runtime {
                message: format!("missing in-flight entry for handle `{handle_id}`"),
                stage: "agent-loop-update-output-summary".to_string(),
                inflight_snapshot: Some(Box::new(self.snapshot().into())),
            })
    }

    /// Updates one handle structured output and fails with structured context if the registry is inconsistent.
    pub(crate) fn update_structured_output_checked(
        &mut self,
        handle_id: &str,
        structured_output: Option<tools::StructuredToolOutput>,
    ) -> Result<InFlightToolCallEntry> {
        self.update_structured_output(handle_id, structured_output)
            .ok_or_else(|| crate::Error::Runtime {
                message: format!("missing in-flight entry for handle `{handle_id}`"),
                stage: "agent-loop-update-structured-output".to_string(),
                inflight_snapshot: Some(Box::new(self.snapshot().into())),
            })
    }

    /// Updates one handle error summary and fails with structured context if the registry is inconsistent.
    pub(crate) fn update_error_summary_checked(
        &mut self,
        handle_id: &str,
        error_summary: Option<String>,
    ) -> Result<InFlightToolCallEntry> {
        self.update_error_summary(handle_id, error_summary)
            .ok_or_else(|| crate::Error::Runtime {
                message: format!("missing in-flight entry for handle `{handle_id}`"),
                stage: "agent-loop-update-error-summary".to_string(),
                inflight_snapshot: Some(Box::new(self.snapshot().into())),
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
    pub(crate) structured_output: Option<tools::StructuredToolOutput>,
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
    pub(crate) entries: Vec<InFlightToolCallEntry>,
    pub(crate) queued_handles: Vec<String>,
    pub(crate) running_handles: Vec<String>,
    pub(crate) completed_handles: Vec<String>,
    pub(crate) cancelled_handles: Vec<String>,
    pub(crate) failed_handles: Vec<String>,
}
