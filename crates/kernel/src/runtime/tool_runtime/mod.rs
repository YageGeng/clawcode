use llm::{
    completion::{AssistantContent, Message},
    one_or_many::OneOrMany,
};
use snafu::ensure;
use tokio_util::sync::CancellationToken;

mod batching;
mod publisher;

use self::batching::build_tool_execution_batches;
use self::publisher::InFlightEventPublisher;
use super::{
    AgentLoopConfig,
    agent_loop::{ToolBatchSummary, ToolBatchSummaryEntry},
    inflight::{CompletedToolCallQueue, InFlightToolCallRegistry, ToolCallRuntimeSnapshot},
};
use crate::{
    Result,
    session::{SessionId, SessionStore, ThreadId},
    tools::{
        ToolContext,
        executor::{
            ToolExecutionBatchReport, ToolExecutionMode, ToolExecutionQueue, ToolExecutionRequest,
            ToolExecutionResult, ToolExecutor,
        },
        router::ToolRouter,
    },
};

/// Queue of completed tool calls ready to execute after one stream finishes.
#[derive(Debug, Clone)]
pub(crate) struct ToolExecutionPlan {
    pub(crate) message_id: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) queue: CompletedToolCallQueue,
    pub(crate) in_flight: InFlightToolCallRegistry,
}

/// Runtime input needed to execute one tool batch and fold its messages back into the turn state.
#[derive(Debug, Clone)]
struct ToolCallBatch {
    message_id: Option<String>,
    text: Option<String>,
    calls: Vec<ToolExecutionRequest>,
    total_tool_calls: usize,
    max_tool_calls: usize,
    tool_execution_mode: ToolExecutionMode,
    cancellation_token: Option<CancellationToken>,
    tool_context: ToolContext,
}

/// Stable runtime dependencies and mutable turn buffers shared across tool execution batches.
pub(crate) struct ToolExecutionRuntimeInput<'a, S, E>
where
    S: SessionStore + ?Sized,
    E: crate::events::EventSink + ?Sized,
{
    pub(crate) store: &'a S,
    pub(crate) session_id: SessionId,
    pub(crate) thread_id: ThreadId,
    pub(crate) router: &'a ToolRouter,
    pub(crate) events: &'a E,
    pub(crate) working_messages: &'a mut Vec<Message>,
    pub(crate) new_messages: &'a mut Vec<Message>,
}

/// Mutable turn state that tool execution appends to as batches complete.
struct ToolRuntime<'a, S, E>
where
    S: SessionStore + ?Sized,
    E: crate::events::EventSink + ?Sized,
{
    store: &'a S,
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

impl<'a, S, E> ToolRuntime<'a, S, E>
where
    S: SessionStore + ?Sized,
    E: crate::events::EventSink + ?Sized,
{
    /// Builds one runtime facade over the mutable turn state used while draining tool batches.
    fn new(
        input: ToolExecutionRuntimeInput<'a, S, E>,
        iteration: usize,
        in_flight_tool_calls: &'a mut InFlightToolCallRegistry,
        completed_batch_summary: &'a mut ToolBatchSummary,
    ) -> Self {
        let ToolExecutionRuntimeInput {
            store,
            session_id,
            thread_id,
            router,
            events,
            working_messages,
            new_messages,
        } = input;
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
    async fn apply_batch(&mut self, batch: ToolCallBatch) -> Result<usize> {
        let ToolCallBatch {
            message_id,
            text,
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

        self.append_assistant_tool_call_message(message_id, text, &calls)
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
        calls: &[ToolExecutionRequest],
    ) -> Result<()> {
        let mut assistant_content = Vec::new();
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
            .append_message(
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
                .append_message(
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

/// Executes the queue of completed tool calls collected during the last stream iteration.
pub(crate) async fn execute_tool_execution_plan<E>(
    input: ToolExecutionRuntimeInput<'_, impl SessionStore, E>,
    config: &AgentLoopConfig,
    plan: ToolExecutionPlan,
    total_tool_calls: usize,
    iteration: usize,
) -> Result<(usize, ToolCallRuntimeSnapshot, ToolBatchSummary)>
where
    E: crate::events::EventSink,
{
    let ToolExecutionPlan {
        message_id,
        text,
        queue,
        mut in_flight,
    } = plan;
    let execution_requests = in_flight.execution_requests_for_calls(queue.into_calls())?;
    let batches =
        build_tool_execution_batches(input.router, config.tool_execution_mode, execution_requests);
    let total_calls_in_plan = batches.iter().map(|batch| batch.queue.len()).sum::<usize>();
    let mut total_tool_calls = total_tool_calls;

    ensure!(
        total_tool_calls + total_calls_in_plan <= config.max_tool_calls,
        crate::error::RuntimeSnafu {
            message: format!("tool call limit exceeded: {}", config.max_tool_calls),
            stage: "agent-loop-max-tool-calls".to_string(),
            inflight_snapshot: Some(in_flight.snapshot().into()),
        }
    );

    let mut completed_batch_summary = ToolBatchSummary::default();
    let mut runtime = ToolRuntime::new(
        ToolExecutionRuntimeInput {
            store: input.store,
            session_id: input.session_id.clone(),
            thread_id: input.thread_id.clone(),
            router: input.router,
            events: input.events,
            working_messages: input.working_messages,
            new_messages: input.new_messages,
        },
        iteration,
        &mut in_flight,
        &mut completed_batch_summary,
    );
    let mut first_batch = true;

    for batch in batches {
        total_tool_calls = runtime
            .apply_batch(ToolCallBatch {
                message_id: first_batch.then_some(message_id.clone()).flatten(),
                text: if first_batch { text.clone() } else { None },
                calls: batch.queue.into_requests(),
                total_tool_calls,
                max_tool_calls: config.max_tool_calls,
                tool_execution_mode: batch.mode,
                cancellation_token: config.cancellation_token.clone(),
                tool_context: ToolContext::new(input.session_id.clone(), input.thread_id.clone())
                    .with_tool_approval_enforcement(config.enforce_tool_approvals)
                    .with_tool_approval_handler_if_needed(config.tool_approval_handler.clone()),
            })
            .await?;
        first_batch = false;
    }

    debug_assert_eq!(
        in_flight.len(),
        total_calls_in_plan,
        "each ready tool call should produce an in-flight registry entry",
    );
    debug_assert!(
        in_flight
            .entries
            .iter()
            .all(|entry| !matches!(entry.identity(), ("", _, _) | (_, "", _) | (_, _, ""))),
        "in-flight registry entries should always retain stable tool identifiers",
    );

    Ok((
        total_tool_calls,
        in_flight.snapshot().into(),
        completed_batch_summary,
    ))
}

/// Returns true when the runtime error came from executor-level cancellation handling.
fn is_tool_execution_cancelled(error: &crate::Error) -> bool {
    matches!(
        error,
        crate::Error::Runtime { stage, .. } if stage == "tool-executor-cancelled"
    )
}
