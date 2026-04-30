use llm::{completion::Message, usage::Usage};

use crate::{
    Result,
    context::SessionTaskContext,
    events::AgentStage,
    events::{AgentEvent, EventSink, TaskContinuationDecisionTraceEntry},
    model::{AgentModel, ModelRequest},
    runtime::{
        FinalizeTextResponseRequest,
        continuation::{
            AgentLoopConfig, ContinuationHookPhase, apply_hook_decision, run_continuation_hook,
            trace_entry_for_hook_decision,
        },
        finalize_text_response,
        inflight::ToolCallRuntimeSnapshot,
        sampling::{IterationOutcome, collect_stream_response},
        tool::{ToolExecutionRuntimeInput, execute_tool_execution_plan},
    },
    session::{SessionContinuationRequest, SessionId, ThreadId},
    tools::router::ToolRouter,
};

/// One display-friendly summary entry describing a tool call that completed in the last batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBatchSummaryEntry {
    pub handle_id: String,
    pub name: String,
    pub tool_id: String,
    pub tool_call_id: String,
    pub output_summary: String,
}

/// Summary of the most recent tool batch completion that a continuation hook can inspect.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolBatchSummary {
    pub entries: Vec<ToolBatchSummaryEntry>,
}

#[derive(Debug, Clone)]
pub struct LoopResult {
    pub final_text: String,
    pub usage: Usage,
    pub new_messages: Vec<Message>,
    pub iterations: usize,
    pub inflight_snapshot: ToolCallRuntimeSnapshot,
    pub requested_continuation: Option<SessionContinuationRequest>,
    pub continuation_decision_trace: Vec<TaskContinuationDecisionTraceEntry>,
    pub(crate) next_tool_handle_sequence: usize,
}

#[derive(Debug, Clone)]
pub struct AgentLoopRequest {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub system_prompt: Option<String>,
    pub working_messages: Vec<Message>,
    pub next_tool_handle_sequence: usize,
}

/// Runs one turn through repeated sampling iterations until the model can respond.
pub(crate) async fn run_turn<M, E>(
    model: &M,
    store: &SessionTaskContext,
    router: &ToolRouter,
    events: &E,
    config: &AgentLoopConfig,
    request: AgentLoopRequest,
) -> Result<LoopResult>
where
    M: AgentModel,
    E: EventSink,
{
    let AgentLoopRequest {
        session_id,
        thread_id,
        system_prompt,
        mut working_messages,
        mut next_tool_handle_sequence,
    } = request;

    // Track turn-scoped state that accumulates across model iterations until
    // the turn either produces a final response or exhausts the loop budget.
    let mut new_messages = Vec::new();
    let mut usage = Usage::new();
    let mut total_tool_calls = 0usize;
    let mut previous_response_id: Option<String> = None;
    let mut final_inflight_snapshot = ToolCallRuntimeSnapshot::default();
    let mut requested_continuation = None;
    let mut continuation_decision_trace = Vec::new();

    for iteration in 1..=config.max_iterations {
        // Recompute tool definitions for each iteration so dynamic tool routing
        // changes are reflected before the next model request is issued.
        let tool_definitions = router.definitions();
        events
            .publish(AgentEvent::StatusUpdated {
                stage: AgentStage::ModelRequesting,
                message: None,
                iteration: Some(iteration),
                tool_id: None,
                tool_call_id: None,
            })
            .await;
        events
            .publish(AgentEvent::ModelRequested {
                message_count: working_messages.len(),
                tool_count: tool_definitions.len(),
            })
            .await;

        let mut stream = model
            .stream(ModelRequest {
                system_prompt: system_prompt.clone(),
                messages: working_messages.clone(),
                tools: tool_definitions,
                tool_choice: config.tool_choice.clone(),
                previous_response_id: previous_response_id.clone(),
            })
            .await?;
        let iteration_result =
            collect_stream_response(events, iteration, next_tool_handle_sequence, &mut stream)
                .await?;

        // Fold the streamed response into turn-scoped accounting before deciding
        // whether this iteration ends the turn or schedules tool execution.
        usage += iteration_result.usage;
        previous_response_id = iteration_result.message_id.clone();
        next_tool_handle_sequence = iteration_result.in_flight_tool_calls.next_handle_sequence();

        match IterationOutcome::from(iteration_result) {
            IterationOutcome::Respond {
                message_id,
                text,
                reasoning,
            } => {
                // Let continuation hooks inspect the completed turn state before
                // the final assistant text is persisted and returned.
                let hook_decision = run_continuation_hook(
                    config,
                    ContinuationHookPhase::BeforeFinalResponse,
                    iteration,
                    LoopResult {
                        final_text: text.clone(),
                        usage,
                        new_messages: new_messages.clone(),
                        iterations: iteration,
                        inflight_snapshot: final_inflight_snapshot.clone(),
                        requested_continuation: requested_continuation.clone(),
                        continuation_decision_trace: continuation_decision_trace.clone(),
                        next_tool_handle_sequence,
                    },
                    None,
                );
                continuation_decision_trace.push(trace_entry_for_hook_decision(
                    ContinuationHookPhase::BeforeFinalResponse,
                    &hook_decision,
                ));
                requested_continuation =
                    apply_hook_decision(requested_continuation.clone(), hook_decision);
                events
                    .publish(AgentEvent::StatusUpdated {
                        stage: AgentStage::Responding,
                        message: Some(text.clone()),
                        iteration: Some(iteration),
                        tool_id: None,
                        tool_call_id: None,
                    })
                    .await;

                return finalize_text_response(
                    store,
                    events,
                    FinalizeTextResponseRequest {
                        session_id,
                        thread_id: thread_id.clone(),
                        message_id,
                        text,
                        reasoning,
                        usage,
                        new_messages,
                        iteration,
                        inflight_snapshot: final_inflight_snapshot,
                        requested_continuation,
                        continuation_decision_trace,
                        next_tool_handle_sequence,
                    },
                )
                .await;
            }
            IterationOutcome::ContinueWithTools(plan) => {
                // Execute the model-requested tool batch, append the resulting
                // messages, and keep the loop alive for another sampling pass.
                let (updated_total_tool_calls, inflight_snapshot, tool_batch_summary) =
                    execute_tool_execution_plan(
                        ToolExecutionRuntimeInput {
                            store,
                            session_id,
                            thread_id: thread_id.clone(),
                            router,
                            events,
                            working_messages: &mut working_messages,
                            new_messages: &mut new_messages,
                        },
                        config,
                        plan,
                        total_tool_calls,
                        iteration,
                    )
                    .await?;
                total_tool_calls = updated_total_tool_calls;
                final_inflight_snapshot.extend(inflight_snapshot);

                // Hooks can request follow-up continuation after observing the
                // completed batch and the updated turn transcript.
                let hook_decision = run_continuation_hook(
                    config,
                    ContinuationHookPhase::ToolBatchCompleted,
                    iteration,
                    LoopResult {
                        final_text: String::new(),
                        usage,
                        new_messages: new_messages.clone(),
                        iterations: iteration,
                        inflight_snapshot: final_inflight_snapshot.clone(),
                        requested_continuation: requested_continuation.clone(),
                        continuation_decision_trace: continuation_decision_trace.clone(),
                        next_tool_handle_sequence,
                    },
                    Some(tool_batch_summary),
                );
                continuation_decision_trace.push(trace_entry_for_hook_decision(
                    ContinuationHookPhase::ToolBatchCompleted,
                    &hook_decision,
                ));
                requested_continuation =
                    apply_hook_decision(requested_continuation.clone(), hook_decision);
            }
        }
    }

    crate::error::RuntimeSnafu {
        message: format!("max iterations exceeded: {}", config.max_iterations),
        stage: "agent-loop-max-iterations".to_string(),
        inflight_snapshot: (!final_inflight_snapshot.entries.is_empty())
            .then_some(Box::new(final_inflight_snapshot)),
    }
    .fail()
}
