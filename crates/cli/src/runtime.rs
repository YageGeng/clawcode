use std::sync::Arc;

use kernel::{
    Agent, AgentContext, AgentDeps, AgentRunRequest, Result,
    events::{AgentEvent, EventSink, TaskContinuationDecisionTraceEntry},
    model::AgentModel,
    session::{SessionId, SessionStore, ThreadId},
    tools::router::ToolRouter,
};
use tracing::{info, trace};

/// Builds a short prompt preview so tracing stays readable for long requests.
pub fn prompt_preview(prompt: &str) -> String {
    const MAX_CHARS: usize = 80;
    let mut preview = prompt.trim().replace('\n', " ");
    if preview.chars().count() > MAX_CHARS {
        preview = format!("{}...", preview.chars().take(MAX_CHARS).collect::<String>());
    }
    preview
}

/// Formats the continuation decision trace as a compact CLI log field.
fn format_continuation_decision_trace(
    decision_trace: &[TaskContinuationDecisionTraceEntry],
) -> String {
    decision_trace
        .iter()
        .map(|entry| match &entry.source {
            Some(source) => format!("{:?}:{:?}:{:?}", entry.stage, entry.decision, source),
            None => format!("{:?}:{:?}", entry.stage, entry.decision),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Formats the continuation decision trace as a multi-line CLI summary.
fn format_continuation_decision_trace_multiline(
    decision_trace: &[TaskContinuationDecisionTraceEntry],
) -> String {
    if decision_trace.is_empty() {
        return "  (no continuation trace)".to_string();
    }

    decision_trace
        .iter()
        .enumerate()
        .map(|(index, entry)| match &entry.source {
            Some(source) => format!(
                "  {}. {:?} -> {:?} ({:?})",
                index + 1,
                entry.stage,
                entry.decision,
                source
            ),
            None => format!("  {}. {:?} -> {:?}", index + 1, entry.stage, entry.decision),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct TracingEventSink;

#[async_trait::async_trait]
impl EventSink for TracingEventSink {
    /// Emits runtime events into the CLI tracing stream for interactive debugging.
    async fn publish(&self, event: AgentEvent) {
        // Emit the raw event first so `RUST_LOG` alone is enough to inspect the
        // exact runtime protocol without adding a separate CLI mode or flag.
        info!(event = ?event, "agent event");
        match event {
            AgentEvent::RunStarted {
                session_id,
                thread_id,
                input,
            } => {
                info!(session_id, thread_id, prompt = %prompt_preview(&input), "agent run started");
            }
            AgentEvent::StatusUpdated {
                stage,
                message,
                iteration,
                tool_id,
                tool_call_id,
            } => {
                info!(stage = ?stage, iteration, message, tool_id, tool_call_id, "agent status updated");
            }
            AgentEvent::ToolStatusUpdated {
                stage,
                name,
                iteration,
                tool_id,
                tool_call_id,
            } => {
                info!(stage = ?stage, name, iteration, tool_id, tool_call_id, "tool status updated");
            }
            AgentEvent::ModelRequested {
                message_count,
                tool_count,
            } => {
                info!(message_count, tool_count, "requesting model completion");
            }
            AgentEvent::ModelResponseCreated { iteration } => {
                info!(iteration, "model response stream created");
            }
            AgentEvent::ModelTextDelta { text, iteration } => {
                trace!(iteration, text = %text, "model text delta");
            }
            AgentEvent::ModelReasoningSummaryDelta {
                id,
                text,
                summary_index,
                iteration,
            } => {
                info!(iteration, id, summary_index, text = %text, "model reasoning summary delta");
            }
            AgentEvent::ModelReasoningContentDelta {
                id,
                text,
                content_index,
                iteration,
            } => {
                info!(iteration, id, content_index, text = %text, "model reasoning content delta");
            }
            AgentEvent::ModelToolCallNameDelta {
                tool_id,
                tool_call_id,
                delta,
                iteration,
            } => {
                info!(
                    iteration,
                    tool_id, tool_call_id, delta, "model tool call name delta"
                );
            }
            AgentEvent::ModelToolCallArgumentsDelta {
                tool_id,
                tool_call_id,
                delta,
                iteration,
            } => {
                info!(
                    iteration,
                    tool_id, tool_call_id, delta, "model tool call arguments delta"
                );
            }
            AgentEvent::ModelOutputItemAdded { item, iteration } => {
                info!(
                    iteration,
                    item = ?item,
                    "model output item added"
                );
            }
            AgentEvent::ModelOutputItemUpdated { item, iteration } => {
                info!(
                    iteration,
                    item = ?item,
                    "model output item updated"
                );
            }
            AgentEvent::ModelOutputItemDone { item, iteration } => {
                info!(
                    iteration,
                    item = ?item,
                    "model output item completed"
                );
            }
            AgentEvent::ModelStreamCompleted {
                message_id,
                usage,
                iteration,
            } => {
                info!(
                    iteration,
                    message_id,
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    total_tokens = usage.total_tokens,
                    "model stream completed"
                );
            }
            AgentEvent::ToolCallQueued {
                name,
                iteration,
                tool_id,
                tool_call_id,
            } => {
                info!(name, iteration, tool_id, tool_call_id, "tool call queued");
            }
            AgentEvent::ToolCallInFlightRegistered {
                name,
                iteration,
                tool_id,
                tool_call_id,
                handle_id,
            } => {
                info!(
                    name,
                    iteration, tool_id, tool_call_id, handle_id, "tool call registered in-flight"
                );
            }
            AgentEvent::ToolCallInFlightStateUpdated {
                name,
                iteration,
                tool_id,
                tool_call_id,
                handle_id,
                state,
                error_summary,
            } => {
                info!(
                    name,
                    iteration,
                    tool_id,
                    tool_call_id,
                    handle_id,
                    state = ?state,
                    error_summary,
                    "tool call in-flight state updated"
                );
            }
            AgentEvent::ToolCallInFlightSnapshot {
                iteration,
                queued_handles,
                running_handles,
                completed_handles,
                cancelled_handles,
                failed_handles,
            } => {
                info!(
                    iteration,
                    queued = queued_handles.len(),
                    running = running_handles.len(),
                    completed = completed_handles.len(),
                    cancelled = cancelled_handles.len(),
                    failed = failed_handles.len(),
                    "tool call in-flight snapshot"
                );
            }
            AgentEvent::ToolCallRequested {
                name,
                handle_id,
                arguments,
            } => {
                info!(tool = %name, handle_id, arguments = %arguments, "tool requested");
            }
            AgentEvent::ToolCallCompleted {
                name,
                handle_id,
                output,
                structured_output,
            } => {
                if let Some(structured_output) = structured_output {
                    info!(
                        tool = %name,
                        handle_id,
                        output = %output,
                        structured_output = %structured_output,
                        "tool completed"
                    );
                } else {
                    info!(tool = %name, handle_id, output = %output, "tool completed");
                }
            }
            AgentEvent::TextProduced { text } => {
                info!(text = %text, "model produced final text");
            }
            AgentEvent::TaskContinuationDecided {
                turn_index,
                action,
                source,
                decision_trace,
            } => {
                let trace = format_continuation_decision_trace(&decision_trace);
                let trace_pretty = format_continuation_decision_trace_multiline(&decision_trace);
                info!(
                    turn_index,
                    action = ?action,
                    source = ?source,
                    trace_len = decision_trace.len(),
                    trace = %trace,
                    trace_pretty = %trace_pretty,
                    "task continuation decided"
                );
            }
            AgentEvent::RunFinished { text, usage } => {
                info!(
                    text = %text,
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    total_tokens = usage.total_tokens,
                    "agent run finished"
                );
            }
        }
    }
}

/// Runs one CLI prompt through the kernel runtime so CLI and kernel share the same path.
pub async fn run_cli_prompt<M, S>(
    model: Arc<M>,
    store: Arc<S>,
    router: Arc<ToolRouter>,
    prompt: String,
) -> Result<String>
where
    M: AgentModel + 'static,
    S: SessionStore + 'static,
{
    let agent = Agent::new(
        AgentContext::new(SessionId::new(), ThreadId::new()),
        AgentDeps::new(model, store, router, Arc::new(TracingEventSink)),
    )
    .with_system_prompt(
        "You are a helpful agent. Use `apply_patch` for file edits and use `exec_command` or `write_stdin` only when command execution is required. Keep file changes inside the workspace, avoid paths containing `..`, and answer directly only when no tool action is needed.",
    );

    let result = agent.run(AgentRunRequest::new(prompt)).await?;
    Ok(result.text)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use kernel::{
        Result,
        events::{
            TaskContinuationDecisionKind, TaskContinuationDecisionStage,
            TaskContinuationDecisionTraceEntry, TaskContinuationSource,
        },
        model::{AgentModel, ModelRequest, ModelResponse},
        session::InMemorySessionStore,
        tools::ToolRouter,
    };
    use llm::usage::Usage;

    use super::{
        format_continuation_decision_trace, format_continuation_decision_trace_multiline,
        run_cli_prompt,
    };

    struct StubAgentModel;

    #[async_trait(?Send)]
    impl AgentModel for StubAgentModel {
        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse> {
            Ok(ModelResponse::text(
                "hello from runtime",
                Usage {
                    input_tokens: 1,
                    output_tokens: 2,
                    total_tokens: 3,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
            ))
        }
    }

    #[tokio::test]
    async fn runs_prompt_through_kernel_agent_runtime() {
        let text = run_cli_prompt(
            Arc::new(StubAgentModel),
            Arc::new(InMemorySessionStore::default()),
            Arc::new(ToolRouter::new(
                Arc::new(kernel::tools::ToolRegistry::default()),
                Vec::new(),
            )),
            "say hello".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(text, "hello from runtime");
    }

    #[test]
    fn formats_continuation_decision_trace_for_cli_logs() {
        let trace = format_continuation_decision_trace(&[
            TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::ToolBatchCompletedHook,
                decision: TaskContinuationDecisionKind::Request,
                source: Some(TaskContinuationSource::SystemFollowUp),
            },
            TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::FinalDecision,
                decision: TaskContinuationDecisionKind::Adopted,
                source: Some(TaskContinuationSource::SystemFollowUp),
            },
        ]);

        assert_eq!(
            trace,
            "ToolBatchCompletedHook:Request:SystemFollowUp | FinalDecision:Adopted:SystemFollowUp"
        );
    }

    #[test]
    fn formats_continuation_decision_trace_for_human_readable_cli_logs() {
        let trace = format_continuation_decision_trace_multiline(&[
            TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::ToolBatchCompletedHook,
                decision: TaskContinuationDecisionKind::Request,
                source: Some(TaskContinuationSource::SystemFollowUp),
            },
            TaskContinuationDecisionTraceEntry {
                stage: TaskContinuationDecisionStage::FinalDecision,
                decision: TaskContinuationDecisionKind::Adopted,
                source: Some(TaskContinuationSource::SystemFollowUp),
            },
        ]);

        assert_eq!(
            trace,
            "  1. ToolBatchCompletedHook -> Request (SystemFollowUp)\n  2. FinalDecision -> Adopted (SystemFollowUp)"
        );
    }
}
