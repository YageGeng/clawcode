use snafu::ensure;

mod batching;
mod plan;
mod publisher;
mod runtime;

use self::batching::build_tool_execution_batches;
pub(crate) use self::plan::{ToolExecutionPlan, ToolExecutionRuntimeInput};
use self::{plan::ToolCallBatch, runtime::ToolRuntime};
use crate::runtime::{
    continuation::AgentLoopConfig, inflight::ToolCallRuntimeSnapshot, turn::ToolBatchSummary,
};
use crate::{Result, session::SessionStore, tools::ToolContext};

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
        input.store,
        input.session_id.clone(),
        input.thread_id.clone(),
        input.router,
        input.events,
        iteration,
        &mut in_flight,
        input.working_messages,
        input.new_messages,
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
