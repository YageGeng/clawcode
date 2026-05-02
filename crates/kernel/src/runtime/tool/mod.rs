use snafu::ensure;

mod batching;
mod plan;
mod publisher;
mod runtime;

use self::batching::build_tool_execution_batches;
pub(crate) use self::plan::{ToolExecutionPlan, ToolExecutionRuntimeInput};
use self::{plan::ToolCallBatch, runtime::ToolRuntime};
use crate::{
    Result,
    runtime::{
        continuation::AgentLoopConfig, inflight::ToolCallRuntimeSnapshot, turn::ToolBatchSummary,
    },
    tools::ToolContext,
};

/// Executes the queue of completed tool calls collected during the last stream iteration.
pub(crate) async fn execute_tool_execution_plan<E>(
    input: ToolExecutionRuntimeInput<'_, E>,
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
        reasoning,
        queue,
        mut in_flight,
    } = plan;
    let agent_runtime_context = input
        .turn_context
        .to_agent_runtime_context(config.max_subagent_depth);
    let execution_requests = in_flight.execution_requests_for_calls(queue.into_calls())?;
    let batches = build_tool_execution_batches(
        input.router,
        &agent_runtime_context,
        config.tool_execution_mode,
        execution_requests,
    );
    let total_calls_in_plan = batches.iter().map(|batch| batch.queue.len()).sum::<usize>();
    let mut total_tool_calls = total_tool_calls;

    ensure!(
        total_tool_calls + total_calls_in_plan <= config.max_tool_calls,
        crate::error::RuntimeSnafu {
            message: format!("tool call limit exceeded: {}", config.max_tool_calls),
            stage: "agent-loop-max-tool-calls".to_string(),
            inflight_snapshot: Some(Box::new(in_flight.snapshot().into())),
        }
    );

    let mut completed_batch_summary = ToolBatchSummary::default();
    let mut runtime = ToolRuntime::new(
        input.store,
        input.session_id,
        input.thread_id.clone(),
        input.router,
        input.events,
        iteration,
        &mut in_flight,
        input.working_messages,
        input.new_messages,
        &mut completed_batch_summary,
    );
    let mut batch_iter = batches.into_iter();
    if let Some(first) = batch_iter.next() {
        total_tool_calls = runtime
            .apply_batch(ToolCallBatch {
                message_id: message_id.clone(),
                text: text.clone(),
                reasoning: reasoning.clone(),
                calls: first.queue.into_requests(),
                total_tool_calls,
                max_tool_calls: config.max_tool_calls,
                tool_execution_mode: first.mode,
                cancellation_token: config.cancellation_token.clone().unwrap_or_default(),
                tool_context: ToolContext::new(input.session_id, input.thread_id.clone())
                    .with_agent_runtime_context(agent_runtime_context.clone())
                    .with_tool_approval_profile(config.tool_approval_profile)
                    .with_tool_approval_handler_if_needed(config.tool_approval_handler.clone())
                    .with_collaboration_runtime_if_needed(input.collaboration_runtime.clone()),
            })
            .await?;

        for batch in batch_iter {
            total_tool_calls = runtime
                .apply_batch(ToolCallBatch {
                    message_id: None,
                    text: None,
                    reasoning: Vec::new(),
                    calls: batch.queue.into_requests(),
                    total_tool_calls,
                    max_tool_calls: config.max_tool_calls,
                    tool_execution_mode: batch.mode,
                    cancellation_token: config.cancellation_token.clone().unwrap_or_default(),
                    tool_context: ToolContext::new(input.session_id, input.thread_id.clone())
                        .with_agent_runtime_context(agent_runtime_context.clone())
                        .with_tool_approval_profile(config.tool_approval_profile)
                        .with_tool_approval_handler_if_needed(config.tool_approval_handler.clone())
                        .with_collaboration_runtime_if_needed(input.collaboration_runtime.clone()),
                })
                .await?;
        }
    }

    debug_assert_eq!(
        in_flight.len(),
        total_calls_in_plan,
        "each ready tool call should produce an in-flight registry entry",
    );
    Ok((
        total_tool_calls,
        in_flight.snapshot().into(),
        completed_batch_summary,
    ))
}
