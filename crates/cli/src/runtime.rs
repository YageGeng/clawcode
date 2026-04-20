use std::sync::Arc;

use kernel::{
    Agent, AgentContext, AgentDeps, AgentRunRequest, Result,
    events::{AgentEvent, EventSink},
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

struct TracingEventSink;

#[async_trait::async_trait]
impl EventSink for TracingEventSink {
    /// Emits runtime events into the CLI tracing stream for interactive debugging.
    async fn publish(&self, event: AgentEvent) {
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
            AgentEvent::ToolCallRequested { name, arguments } => {
                info!(tool = %name, arguments = %arguments, "tool requested");
            }
            AgentEvent::ToolCallCompleted {
                name,
                output,
                structured_output,
            } => {
                if let Some(structured_output) = structured_output {
                    info!(
                        tool = %name,
                        output = %output,
                        structured_output = %structured_output,
                        "tool completed"
                    );
                } else {
                    info!(tool = %name, output = %output, "tool completed");
                }
            }
            AgentEvent::TextProduced { text } => {
                info!(text = %text, "model produced final text");
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
        model::{AgentModel, ModelRequest, ModelResponse},
        session::InMemorySessionStore,
        tools::ToolRouter,
    };
    use llm::usage::Usage;

    use super::run_cli_prompt;

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
}
