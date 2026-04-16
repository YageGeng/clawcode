use std::sync::Arc;

use kernel::{
    Agent, AgentContext, AgentDeps, AgentRunRequest, Result,
    events::{AgentEvent, EventSink},
    model::AgentModel,
    session::{SessionId, SessionStore, ThreadId},
    tools::registry::ToolRegistry,
};
use tracing::info;

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
    registry: Arc<ToolRegistry>,
    prompt: String,
) -> Result<String>
where
    M: AgentModel + 'static,
    S: SessionStore + 'static,
{
    let agent = Agent::new(
        AgentContext::new(SessionId::new(), ThreadId::new()),
        AgentDeps::new(model, store, registry, Arc::new(TracingEventSink)),
    )
    .with_system_prompt(
        "You are a helpful agent. Use tools whenever the user explicitly asks to read or write files. For file writes, call the write tool directly instead of asking for confirmation. Relative nested paths such as `./doc/example.md` are allowed under the tool root, and missing parent directories are created automatically. Paths containing `..` are not allowed. Answer directly only when no tool action is needed.",
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
        tools::registry::ToolRegistry,
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
            Arc::new(ToolRegistry::default()),
            "say hello".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(text, "hello from runtime");
    }
}
