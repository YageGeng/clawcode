use llm::{
    completion::{AssistantContent, Message},
    one_or_many::OneOrMany,
};
use snafu::ensure;
use tokio_util::sync::CancellationToken;

use super::{ToolCallBatch, publisher::InFlightEventPublisher};
use crate::{
    Result,
    context::SessionTaskContext,
    runtime::{
        ToolBatchSummary, ToolBatchSummaryEntry,
        inflight::{InFlightToolCallRegistry, ToolCallRuntimeSnapshot},
    },
    session::{SessionId, ThreadId},
    tools::{
        ToolContext,
        executor::{
            ToolExecutionBatchReport, ToolExecutionMode, ToolExecutionQueue, ToolExecutionRequest,
            ToolExecutionResult, ToolExecutor,
        },
        router::ToolRouter,
    },
};

/// Mutable turn state that tool execution appends to as batches complete.
pub(super) struct ToolRuntime<'a, E>
where
    E: crate::events::EventSink + ?Sized,
{
    store: &'a SessionTaskContext,
    session_id: SessionId,
    thread_id: ThreadId,
    router: &'a ToolRouter,
    events: &'a E,
    iteration: usize,
    in_flight_tool_calls: &'a mut InFlightToolCallRegistry,
    working_messages: &'a mut Vec<Message>,
    new_messages: &'a mut Vec<Message>,
    completed_batch_summary: &'a mut ToolBatchSummary,
}

impl<'a, E> ToolRuntime<'a, E>
where
    E: crate::events::EventSink + ?Sized,
{
    /// Builds one runtime facade over the mutable turn state used while draining tool batches.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        store: &'a SessionTaskContext,
        session_id: SessionId,
        thread_id: ThreadId,
        router: &'a ToolRouter,
        events: &'a E,
        iteration: usize,
        in_flight_tool_calls: &'a mut InFlightToolCallRegistry,
        working_messages: &'a mut Vec<Message>,
        new_messages: &'a mut Vec<Message>,
        completed_batch_summary: &'a mut ToolBatchSummary,
    ) -> Self {
        Self {
            store,
            session_id,
            thread_id,
            router,
            events,
            iteration,
            in_flight_tool_calls,
            working_messages,
            new_messages,
            completed_batch_summary,
        }
    }

    /// Creates a publisher bound to the current iteration's in-flight registry.
    fn publisher(&mut self) -> InFlightEventPublisher<'_, E> {
        InFlightEventPublisher::new(self.events, self.in_flight_tool_calls, self.iteration)
    }

    /// Returns the current grouped runtime snapshot for structured runtime errors.
    fn inflight_snapshot(&self) -> ToolCallRuntimeSnapshot {
        self.in_flight_tool_calls.snapshot().into()
    }

    /// Returns every handle currently marked as running.
    fn running_handle_ids(&self) -> Vec<String> {
        self.in_flight_tool_calls
            .entries
            .iter()
            .filter(|entry| entry.state == crate::events::ToolCallInFlightState::Running)
            .map(|entry| entry.handle_id.clone())
            .collect()
    }

    /// Appends the assistant's tool-call message and drains the selected execution batch.
    pub(super) async fn apply_batch(&mut self, batch: ToolCallBatch) -> Result<usize> {
        let ToolCallBatch {
            message_id,
            text,
            reasoning,
            calls,
            total_tool_calls,
            max_tool_calls,
            tool_execution_mode,
            cancellation_token,
            tool_context,
        } = batch;
        let call_count = calls.len();

        ensure!(
            total_tool_calls + call_count <= max_tool_calls,
            crate::error::RuntimeSnafu {
                message: format!("tool call limit exceeded: {max_tool_calls}"),
                stage: "agent-loop-max-tool-calls".to_string(),
                inflight_snapshot: Some(self.inflight_snapshot()),
            }
        );

        self.append_assistant_tool_call_message(message_id, text, reasoning, &calls)
            .await?;

        if tool_execution_mode == ToolExecutionMode::Serial {
            self.execute_serial(calls, tool_context, cancellation_token.unwrap_or_default())
                .await?;
        } else {
            self.execute_parallel(calls, tool_context, cancellation_token.unwrap_or_default())
                .await?;
        }

        Ok(total_tool_calls + call_count)
    }

    /// Persists the assistant tool-call message that precedes tool execution results.
    async fn append_assistant_tool_call_message(
        &mut self,
        message_id: Option<String>,
        text: Option<String>,
        reasoning: Vec<llm::completion::message::Reasoning>,
        calls: &[ToolExecutionRequest],
    ) -> Result<()> {
        let mut assistant_content = Vec::new();
        assistant_content.extend(
            reasoning
                .into_iter()
                .map(llm::completion::message::AssistantContent::Reasoning),
        );
        if let Some(text) = text {
            assistant_content.push(AssistantContent::text(text));
        }

        for call in calls {
            let call_id = call
                .call
                .call_id
                .clone()
                .unwrap_or_else(|| call.call.id.clone());
            assistant_content.push(AssistantContent::tool_call_with_call_id(
                call.call.id.clone(),
                call_id,
                call.call.name.clone(),
                call.call.arguments.clone(),
            ));
        }

        let assistant_message = Message::Assistant {
            id: message_id,
            content: OneOrMany::many(assistant_content).map_err(|_| crate::Error::Runtime {
                message: "assistant tool-call content cannot be empty".to_string(),
                stage: "agent-loop-build-tool-call-message".to_string(),
                inflight_snapshot: Some(self.inflight_snapshot()),
            })?,
        };
        self.store
            .append_message_state(
                self.session_id.clone(),
                self.thread_id.clone(),
                assistant_message.clone(),
            )
            .await?;
        self.working_messages.push(assistant_message.clone());
        self.new_messages.push(assistant_message);
        Ok(())
    }

    /// Executes one serial batch and updates in-flight state only when each call actually starts.
    async fn execute_serial(
        &mut self,
        calls: Vec<ToolExecutionRequest>,
        tool_context: ToolContext,
        cancellation_token: CancellationToken,
    ) -> Result<()> {
        for call in calls {
            self.publisher().publish_execution_started(&call).await?;
            let execution_report =
                match ToolExecutor::execute_queue_report_with_mode_and_cancellation(
                    self.router,
                    ToolExecutionQueue::from_requests(vec![call]),
                    tool_context.clone(),
                    ToolExecutionMode::Serial,
                    cancellation_token.clone(),
                )
                .await
                {
                    Ok(report) => report,
                    Err(error) => {
                        self.handle_batch_dispatch_error(&error).await?;
                        return Err(error.with_inflight_snapshot(self.inflight_snapshot()));
                    }
                };
            self.handle_execution_report(execution_report).await?;
        }

        Ok(())
    }

    /// Executes one parallel-safe batch after marking all calls as running together.
    async fn execute_parallel(
        &mut self,
        calls: Vec<ToolExecutionRequest>,
        tool_context: ToolContext,
        cancellation_token: CancellationToken,
    ) -> Result<()> {
        for call in &calls {
            self.publisher().publish_execution_started(call).await?;
        }

        let execution_report = match ToolExecutor::execute_queue_report_with_mode_and_cancellation(
            self.router,
            ToolExecutionQueue::from_requests(calls),
            tool_context,
            ToolExecutionMode::Parallel,
            cancellation_token,
        )
        .await
        {
            Ok(report) => report,
            Err(error) => {
                self.handle_batch_dispatch_error(&error).await?;
                return Err(error.with_inflight_snapshot(self.inflight_snapshot()));
            }
        };

        self.handle_execution_report(execution_report).await
    }

    /// Handles dispatcher-level batch errors such as cancellation before a report is produced.
    async fn handle_batch_dispatch_error(&mut self, error: &crate::Error) -> Result<()> {
        if is_tool_execution_cancelled(error) {
            self.publisher().publish_cancellation_updates().await?;
        } else {
            let running_handle_ids = self.running_handle_ids();
            self.publisher()
                .publish_failure_updates(running_handle_ids, error.to_string())
                .await?;
        }
        Ok(())
    }

    /// Applies a batch report by recording any completed tool results and surfacing later failures.
    async fn handle_execution_report(
        &mut self,
        execution_report: ToolExecutionBatchReport,
    ) -> Result<()> {
        match execution_report {
            ToolExecutionBatchReport::Completed(results) => {
                self.append_completed_tool_results(results).await
            }
            ToolExecutionBatchReport::Failed(failure) => {
                let failure = *failure;
                self.append_completed_tool_results(failure.completed_results)
                    .await?;
                self.in_flight_tool_calls.update_output_summary_checked(
                    &failure.failed_result.handle_id,
                    Some(failure.failed_result.output.text.clone()),
                )?;
                self.in_flight_tool_calls.update_structured_output_checked(
                    &failure.failed_result.handle_id,
                    Some(failure.failed_result.output.structured.clone()),
                )?;
                self.publisher()
                    .publish_failure_updates(
                        vec![failure.failed_request.handle_id.clone()],
                        failure.error.to_string(),
                    )
                    .await?;
                Err(failure
                    .error
                    .with_inflight_snapshot(self.inflight_snapshot()))
            }
        }
    }

    /// Records completed tool results before the loop proceeds or surfaces a later batch failure.
    async fn append_completed_tool_results(
        &mut self,
        results: Vec<ToolExecutionResult>,
    ) -> Result<()> {
        for result in results {
            let tool_call_id = result
                .call
                .call_id
                .clone()
                .unwrap_or_else(|| result.call.id.clone());
            self.in_flight_tool_calls.update_output_summary_checked(
                &result.handle_id,
                Some(result.output.text.clone()),
            )?;
            self.in_flight_tool_calls.update_structured_output_checked(
                &result.handle_id,
                Some(result.output.structured.clone()),
            )?;
            self.publisher()
                .publish_completed(&result.call.name, &result.handle_id)
                .await?;
            self.events
                .publish(crate::events::AgentEvent::ToolCallCompleted {
                    name: result.call.name.clone(),
                    handle_id: result.handle_id.clone(),
                    output: result.output.text.clone(),
                    structured_output: Some(result.output.structured.clone()),
                })
                .await;
            self.events
                .publish(crate::events::AgentEvent::ToolStatusUpdated {
                    stage: crate::events::ToolStage::Completed,
                    name: result.call.name.clone(),
                    iteration: Some(self.iteration),
                    tool_id: result.call.id.clone(),
                    tool_call_id: tool_call_id.clone(),
                })
                .await;
            self.store
                .append_message_state(
                    self.session_id.clone(),
                    self.thread_id.clone(),
                    result.message.clone(),
                )
                .await?;
            self.working_messages.push(result.message.clone());
            self.new_messages.push(result.message);
            self.completed_batch_summary
                .entries
                .push(ToolBatchSummaryEntry {
                    handle_id: result.handle_id,
                    name: result.call.name,
                    tool_id: result.call.id,
                    tool_call_id,
                    output_summary: result.output.text,
                });
        }

        Ok(())
    }
}

/// Returns true when the runtime error came from executor-level cancellation handling.
pub(super) fn is_tool_execution_cancelled(error: &crate::Error) -> bool {
    matches!(
        error,
        crate::Error::Runtime { stage, .. } if stage == "tool-executor-cancelled"
    )
}
