use crate::{
    Result,
    events::{AgentEvent, EventSink, ToolCallInFlightState, ToolStage},
    tools::executor::ToolExecutionRequest,
};
use tracing::info;

use super::super::inflight::InFlightToolCallRegistry;

/// Publishes in-flight lifecycle events while mutating the shared tool-call registry.
pub(super) struct InFlightEventPublisher<'a, E>
where
    E: EventSink + ?Sized,
{
    events: &'a E,
    in_flight_tool_calls: &'a mut InFlightToolCallRegistry,
    iteration: usize,
}

impl<'a, E> InFlightEventPublisher<'a, E>
where
    E: EventSink + ?Sized,
{
    /// Builds a publisher bound to one iteration's registry and event sink.
    pub(super) fn new(
        events: &'a E,
        in_flight_tool_calls: &'a mut InFlightToolCallRegistry,
        iteration: usize,
    ) -> Self {
        Self {
            events,
            in_flight_tool_calls,
            iteration,
        }
    }

    /// Updates one handle state and emits both the state delta and grouped snapshot.
    pub(super) async fn publish_state_update(
        &mut self,
        name: &str,
        handle_id: &str,
        state: ToolCallInFlightState,
        error_summary: Option<String>,
    ) -> Result<()> {
        let entry = self
            .in_flight_tool_calls
            .update_state_checked(handle_id, state.clone())?;
        let entry = if error_summary.is_some() {
            self.in_flight_tool_calls
                .update_error_summary_checked(handle_id, error_summary.clone())?
        } else {
            entry
        };

        self.events
            .publish(AgentEvent::ToolCallInFlightStateUpdated {
                name: if entry.name.is_empty() {
                    name.to_string()
                } else {
                    entry.name
                },
                iteration: Some(self.iteration),
                tool_id: entry.tool_id,
                tool_call_id: entry.tool_call_id,
                handle_id: entry.handle_id,
                state,
                error_summary,
            })
            .await;
        self.publish_snapshot().await;
        Ok(())
    }

    /// Emits the grouped handle-level runtime snapshot after an in-flight state transition.
    pub(super) async fn publish_snapshot(&self) {
        let snapshot = self.in_flight_tool_calls.snapshot();
        self.events
            .publish(AgentEvent::ToolCallInFlightSnapshot {
                iteration: Some(self.iteration),
                queued_handles: snapshot.queued_handles,
                running_handles: snapshot.running_handles,
                completed_handles: snapshot.completed_handles,
                cancelled_handles: snapshot.cancelled_handles,
                failed_handles: snapshot.failed_handles,
            })
            .await;
    }

    /// Marks every non-terminal tool call in the registry as cancelled and emits matching events.
    pub(super) async fn publish_cancellation_updates(&mut self) -> Result<()> {
        for entry in self.in_flight_tool_calls.active_entries() {
            self.publish_state_update(
                &entry.name,
                &entry.handle_id,
                ToolCallInFlightState::Cancelled,
                None,
            )
            .await?;
        }
        Ok(())
    }

    /// Marks selected tool calls as failed and records the shared runtime error summary.
    pub(super) async fn publish_failure_updates(
        &mut self,
        failed_handle_ids: Vec<String>,
        error_summary: String,
    ) -> Result<()> {
        let failed_meta = self
            .in_flight_tool_calls
            .names_for_handles(&failed_handle_ids);
        for (name, handle_id) in failed_meta {
            self.publish_state_update(
                &name,
                &handle_id,
                ToolCallInFlightState::Failed,
                Some(error_summary.clone()),
            )
            .await?;
        }
        Ok(())
    }

    /// Publishes the runtime events that mark one tool call as actively starting execution.
    pub(super) async fn publish_execution_started(
        &mut self,
        call: &ToolExecutionRequest,
    ) -> Result<()> {
        let tool_call_id = call
            .call
            .call_id
            .clone()
            .unwrap_or_else(|| call.call.id.clone());
        self.publish_state_update(
            &call.call.name,
            &call.handle_id,
            ToolCallInFlightState::Running,
            None,
        )
        .await?;
        info!(
            name = %call.call.name,
            handle_id = %call.handle_id,
            "tool call started"
        );
        self.events
            .publish(AgentEvent::ToolStatusUpdated {
                stage: ToolStage::Calling,
                name: call.call.name.clone(),
                iteration: Some(self.iteration),
                tool_id: call.call.id.clone(),
                tool_call_id,
            })
            .await;
        self.events
            .publish(AgentEvent::ToolCallRequested {
                name: call.call.name.clone(),
                handle_id: call.handle_id.clone(),
                arguments: call.call.arguments.clone(),
            })
            .await;
        Ok(())
    }

    /// Marks one handle as completed and emits the matching in-flight state transition.
    pub(super) async fn publish_completed(&mut self, name: &str, handle_id: &str) -> Result<()> {
        self.publish_state_update(name, handle_id, ToolCallInFlightState::Completed, None)
            .await
    }
}
