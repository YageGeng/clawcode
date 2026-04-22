use llm::usage::Usage;

use crate::{
    Result,
    context::SessionTaskContext,
    events::{AgentEvent, EventSink, TaskContinuationDecisionTraceEntry},
    model::AgentModel,
    runtime::{
        ToolCallRuntimeSnapshot,
        continuation::{AgentLoopConfig, decide_task_continuation},
        turn::{TurnExecutionRequest, run_persisted_turn},
    },
    tools::router::ToolRouter,
};

use super::{RunFailure, RunOutcome, RunRequest, RunResult};

/// Executes one runtime task and wraps the inner turn result into the public outcome type.
pub(crate) async fn run_task<M, E>(
    model: &M,
    store: &SessionTaskContext,
    router: &ToolRouter,
    events: &E,
    config: &AgentLoopConfig,
    system_prompt: Option<String>,
    request: RunRequest,
) -> Result<RunOutcome>
where
    M: AgentModel + 'static,
    E: EventSink + 'static,
{
    events
        .publish(AgentEvent::RunStarted {
            session_id: request.session_id.to_string(),
            thread_id: request.thread_id.to_string(),
            input: request.input.clone(),
        })
        .await;

    // The outer task loop may execute multiple persisted turns when the runtime
    // decides to continue with pending input or an internal follow-up request.
    let mut next_turn_request = Some(request);

    // Aggregate task-level state so callers can inspect the whole run instead
    // of only the final turn.
    let mut total_usage = Usage::new();
    let mut total_iterations = 0usize;
    let mut turn_index = 0usize;
    let mut task_inflight_snapshot = ToolCallRuntimeSnapshot::default();
    let mut task_continuation_decision_trace = Vec::new();

    // Preserve tool handle numbering across turns so task-scoped tracing remains
    // stable even when the task spans multiple persisted turns.
    let mut next_tool_handle_sequence = 0usize;

    while let Some(turn_request) = next_turn_request.take() {
        turn_index += 1;

        // Execute exactly one persisted turn, including store lifecycle hooks
        // such as begin_turn and cleanup on failure.
        let turn_result = run_persisted_turn(
            model,
            store,
            router,
            events,
            config,
            TurnExecutionRequest {
                request: turn_request.clone(),
                system_prompt: system_prompt.clone(),
                next_tool_handle_sequence,
            },
        )
        .await;
        let loop_result = match turn_result {
            Ok(loop_result) => loop_result,
            Err(error) => {
                return Ok(build_task_failure_outcome(
                    error,
                    &task_inflight_snapshot,
                    task_continuation_decision_trace,
                ));
            }
        };

        // Merge the completed turn into the task-level aggregates that will be
        // returned on success or attached to failures.
        total_usage += loop_result.usage;
        total_iterations += loop_result.iterations;
        next_tool_handle_sequence = loop_result.next_tool_handle_sequence;
        task_inflight_snapshot.extend(loop_result.inflight_snapshot.clone());

        let (continuation, mut decision_trace) =
            match decide_task_continuation(store, &turn_request, &loop_result, config).await {
                Ok(decision) => decision,
                Err(error) => {
                    let continuation_decision_trace = merge_continuation_traces(
                        task_continuation_decision_trace,
                        loop_result.continuation_decision_trace.clone(),
                    );
                    let error =
                        preserve_original_error_after_task_cleanup(store, &turn_request, error)
                            .await;
                    return Ok(build_task_failure_outcome(
                        error,
                        &task_inflight_snapshot,
                        continuation_decision_trace,
                    ));
                }
            };
        decision_trace.push(TaskContinuationDecisionTraceEntry::from(&continuation));
        task_continuation_decision_trace.extend(decision_trace.clone());
        events
            .publish(AgentEvent::TaskContinuationDecided {
                turn_index,
                action: continuation.action(),
                source: continuation.source(),
                decision_trace,
            })
            .await;

        // The continuation decision determines whether this task ends now or
        // gets translated into the next persisted turn request.
        let next_request = continuation.into_run_request(
            turn_request.session_id.clone(),
            turn_request.thread_id.clone(),
        );

        match next_request {
            Some(next_request) => {
                if let Err(error) = store
                    .finalize_turn_by_id(
                        turn_request.session_id.clone(),
                        turn_request.thread_id.clone(),
                        loop_result.usage,
                    )
                    .await
                {
                    let error =
                        preserve_original_error_after_task_cleanup(store, &turn_request, error)
                            .await;
                    return Ok(build_task_failure_outcome(
                        error,
                        &task_inflight_snapshot,
                        task_continuation_decision_trace,
                    ));
                }

                // Keep looping with the synthesized follow-up input.
                next_turn_request = Some(next_request);
            }
            None => {
                if let Err(error) = store
                    .finalize_turn_by_id(
                        turn_request.session_id.clone(),
                        turn_request.thread_id.clone(),
                        loop_result.usage,
                    )
                    .await
                {
                    let error =
                        preserve_original_error_after_task_cleanup(store, &turn_request, error)
                            .await;
                    return Ok(build_task_failure_outcome(
                        error,
                        &task_inflight_snapshot,
                        task_continuation_decision_trace,
                    ));
                }

                events
                    .publish(AgentEvent::RunFinished {
                        text: loop_result.final_text.clone(),
                        usage: total_usage,
                    })
                    .await;

                return Ok(RunOutcome::Success(RunResult {
                    text: loop_result.final_text,
                    usage: total_usage,
                    iterations: total_iterations,
                    inflight_snapshot: task_inflight_snapshot,
                    continuation_decision_trace: task_continuation_decision_trace,
                }));
            }
        }
    }

    crate::error::RuntimeSnafu {
        message: "task loop exited without a continuation decision".to_string(),
        stage: "runner-task-loop".to_string(),
        inflight_snapshot: None,
    }
    .fail()
}

/// Packages one task-level failure so callers always receive the aggregated runtime snapshot.
fn build_task_failure_outcome(
    error: crate::Error,
    task_inflight_snapshot: &ToolCallRuntimeSnapshot,
    continuation_decision_trace: Vec<TaskContinuationDecisionTraceEntry>,
) -> RunOutcome {
    let mut inflight_snapshot = task_inflight_snapshot.clone();
    let error_snapshot = match &error {
        crate::Error::Runtime {
            inflight_snapshot: Some(snapshot),
            ..
        }
        | crate::Error::Tool {
            inflight_snapshot: Some(snapshot),
            ..
        }
        | crate::Error::Cleanup {
            inflight_snapshot: Some(snapshot),
            ..
        } => Some(snapshot.clone()),
        _ => None,
    };
    if let Some(error_snapshot) = error_snapshot {
        inflight_snapshot.extend(error_snapshot);
    }

    RunOutcome::Failure(RunFailure {
        error,
        inflight_snapshot,
        continuation_decision_trace,
    })
}

/// Merges task-level trace entries with the current turn's trace entries without losing either side.
fn merge_continuation_traces(
    mut task_trace: Vec<TaskContinuationDecisionTraceEntry>,
    turn_trace: Vec<TaskContinuationDecisionTraceEntry>,
) -> Vec<TaskContinuationDecisionTraceEntry> {
    task_trace.extend(turn_trace);
    task_trace
}

/// Tries to discard the active turn after a post-loop task failure while preserving the primary error.
pub(crate) async fn preserve_original_error_after_task_cleanup(
    store: &SessionTaskContext,
    request: &RunRequest,
    original_error: crate::Error,
) -> crate::Error {
    let inflight_snapshot = extract_inflight_snapshot(&original_error);
    match discard_active_turn_after_task_failure(store, request).await {
        Ok(()) => original_error,
        Err(cleanup_error) => crate::Error::Cleanup {
            source: Box::new(original_error),
            cleanup_error: Box::new(cleanup_error),
            stage: "runner-discard-active-turn".to_string(),
            inflight_snapshot,
        },
    }
}

/// Extracts any attached runtime snapshot so cleanup wrappers can preserve the original context.
fn extract_inflight_snapshot(error: &crate::Error) -> Option<ToolCallRuntimeSnapshot> {
    match error {
        crate::Error::Runtime {
            inflight_snapshot, ..
        }
        | crate::Error::Tool {
            inflight_snapshot, ..
        }
        | crate::Error::Cleanup {
            inflight_snapshot, ..
        } => inflight_snapshot.clone(),
        _ => None,
    }
}

/// Discards the still-active turn after a post-loop task failure so later runs can start cleanly.
async fn discard_active_turn_after_task_failure(
    store: &SessionTaskContext,
    request: &RunRequest,
) -> Result<()> {
    store
        .discard_turn_state(request.session_id.clone(), request.thread_id.clone())
        .await
}
