use llm::{
    completion::message::ToolChoice,
    completion::{AssistantContent, Message},
    one_or_many::OneOrMany,
    usage::Usage,
};
use snafu::ensure;

use std::fmt;

use crate::{
    Result,
    events::{AgentEvent, EventSink},
    events::{AgentStage, ToolStage},
    model::{AgentModel, ModelOutput, ModelRequest, ModelResponse},
    session::{SessionId, ThreadId},
    tools::{ToolApprovalHandler, ToolContext, executor::ToolExecutor, registry::ToolRegistry},
};

#[derive(Clone)]
pub struct AgentLoopConfig {
    pub max_iterations: usize,
    pub max_tool_calls: usize,
    pub recent_message_limit: usize,
    pub tool_choice: ToolChoice,
    pub enforce_tool_approvals: bool,
    pub tool_approval_handler: Option<ToolApprovalHandler>,
}

impl fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("max_iterations", &self.max_iterations)
            .field("max_tool_calls", &self.max_tool_calls)
            .field("recent_message_limit", &self.recent_message_limit)
            .field("tool_choice", &self.tool_choice)
            .field("enforce_tool_approvals", &self.enforce_tool_approvals)
            .field(
                "tool_approval_handler",
                &self.tool_approval_handler.as_ref().map(|_| "<function>"),
            )
            .finish()
    }
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 8,
            max_tool_calls: 16,
            recent_message_limit: 24,
            tool_choice: ToolChoice::Auto,
            enforce_tool_approvals: false,
            tool_approval_handler: None,
        }
    }
}

impl AgentLoopConfig {
    /// Sets whether tools marked as `ApprovalRequirement::Always` must pass approval.
    pub fn with_tool_approvals(mut self, enforce_tool_approvals: bool) -> Self {
        self.enforce_tool_approvals = enforce_tool_approvals;
        self
    }

    /// Installs an approval hook for tools requiring explicit user confirmation.
    pub fn with_tool_approval_handler(
        mut self,
        handler: impl Fn(&crate::tools::ToolApprovalRequest) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.tool_approval_handler = Some(std::sync::Arc::new(handler));
        self
    }
}

#[derive(Debug, Clone)]
pub struct LoopResult {
    pub final_text: String,
    pub usage: Usage,
    pub new_messages: Vec<Message>,
    pub iterations: usize,
}

#[derive(Debug, Clone)]
pub struct AgentLoopRequest {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub system_prompt: Option<String>,
    pub working_messages: Vec<Message>,
}

#[derive(Debug, Clone)]
struct ToolCallBatch {
    message_id: Option<String>,
    text: Option<String>,
    calls: Vec<crate::tools::ToolCallRequest>,
    total_tool_calls: usize,
    max_tool_calls: usize,
    tool_context: ToolContext,
    iteration: usize,
}

/// Runs the first milestone's completion -> tool -> completion loop.
pub async fn run_agent_loop<M, E>(
    model: &M,
    registry: &ToolRegistry,
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
    } = request;
    let mut new_messages = Vec::new();
    let mut usage = Usage::new();
    let mut total_tool_calls = 0usize;

    for iteration in 1..=config.max_iterations {
        let tool_definitions = registry.definitions().await;
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

        let response = model
            .complete(ModelRequest {
                system_prompt: system_prompt.clone(),
                messages: working_messages.clone(),
                tools: tool_definitions,
                tool_choice: config.tool_choice.clone(),
            })
            .await?;

        let ModelResponse {
            output,
            usage: response_usage,
            message_id,
        } = response;
        usage += response_usage;

        match output {
            ModelOutput::Text(text) => {
                events
                    .publish(AgentEvent::StatusUpdated {
                        stage: AgentStage::Responding,
                        message: Some(text.clone()),
                        iteration: Some(iteration),
                        tool_id: None,
                        tool_call_id: None,
                    })
                    .await;
                return finish_text_response(
                    events,
                    message_id,
                    text,
                    usage,
                    new_messages,
                    iteration,
                )
                .await;
            }
            ModelOutput::ToolCalls { text, calls } => {
                total_tool_calls = apply_tool_calls(
                    registry,
                    events,
                    ToolCallBatch {
                        message_id,
                        text,
                        calls,
                        total_tool_calls,
                        max_tool_calls: config.max_tool_calls,
                        tool_context: ToolContext::new(session_id.clone(), thread_id.clone())
                            .with_tool_approval_enforcement(config.enforce_tool_approvals)
                            .with_tool_approval_handler_if_needed(
                                config.tool_approval_handler.clone(),
                            ),
                        iteration,
                    },
                    &mut working_messages,
                    &mut new_messages,
                )
                .await?;
            }
        }
    }

    crate::error::RuntimeSnafu {
        message: format!("max iterations exceeded: {}", config.max_iterations),
        stage: "agent-loop-max-iterations".to_string(),
    }
    .fail()
}

/// Publishes the final assistant text and packages the completed loop result.
async fn finish_text_response<E>(
    events: &E,
    message_id: Option<String>,
    text: String,
    usage: Usage,
    mut new_messages: Vec<Message>,
    iteration: usize,
) -> Result<LoopResult>
where
    E: EventSink,
{
    events
        .publish(AgentEvent::TextProduced { text: text.clone() })
        .await;
    let assistant = message_id
        .map(|id| Message::assistant_with_id(id, text.clone()))
        .unwrap_or_else(|| Message::assistant(text.clone()));
    new_messages.push(assistant);
    Ok(LoopResult {
        final_text: text,
        usage,
        new_messages,
        iterations: iteration,
    })
}

/// Appends assistant tool calls, executes them, and records tool-result messages.
async fn apply_tool_calls<E>(
    registry: &ToolRegistry,
    events: &E,
    batch: ToolCallBatch,
    working_messages: &mut Vec<Message>,
    new_messages: &mut Vec<Message>,
) -> Result<usize>
where
    E: EventSink,
{
    let ToolCallBatch {
        message_id,
        text,
        calls,
        total_tool_calls,
        max_tool_calls,
        tool_context,
        iteration,
    } = batch;

    let call_count = calls.len();

    ensure!(
        total_tool_calls + call_count <= max_tool_calls,
        crate::error::RuntimeSnafu {
            message: format!("tool call limit exceeded: {max_tool_calls}"),
            stage: "agent-loop-max-tool-calls".to_string(),
        }
    );

    let mut assistant_content = Vec::new();
    let primary_tool_name = calls.first().map(|call| call.name.clone());
    let primary_tool_id = calls.first().map(|call| call.id.clone());

    let primary_tool_call_id = calls
        .first()
        .map(|call| call.call_id.clone().unwrap_or_else(|| call.id.clone()));

    if let Some(text) = text {
        assistant_content.push(AssistantContent::text(text));
    }

    events
        .publish(AgentEvent::ToolStatusUpdated {
            stage: ToolStage::Calling,
            name: primary_tool_name.clone().unwrap_or_default(),
            iteration: Some(iteration),
            tool_id: primary_tool_id.clone().unwrap_or_default(),
            tool_call_id: primary_tool_call_id.clone().unwrap_or_default(),
        })
        .await;

    for call in &calls {
        events
            .publish(AgentEvent::ToolCallRequested {
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .await;
        let call_id = call.call_id.clone().unwrap_or_else(|| call.id.clone());
        assistant_content.push(AssistantContent::tool_call_with_call_id(
            call.id.clone(),
            call_id,
            call.name.clone(),
            call.arguments.clone(),
        ));
    }

    let assistant_message = Message::Assistant {
        id: message_id,
        content: OneOrMany::many(assistant_content)
            .expect("assistant tool-call content should not be empty"),
    };
    working_messages.push(assistant_message.clone());
    new_messages.push(assistant_message);

    let results = ToolExecutor::execute_all(registry, calls, tool_context).await?;
    for result in results {
        events
            .publish(AgentEvent::ToolCallCompleted {
                name: result.call.name.clone(),
                output: result.output.text.clone(),
                structured_output: Some(result.output.structured.clone()),
            })
            .await;
        working_messages.push(result.message.clone());
        new_messages.push(result.message);
    }

    events
        .publish(AgentEvent::ToolStatusUpdated {
            stage: ToolStage::Completed,
            name: primary_tool_name.unwrap_or_default(),
            iteration: Some(iteration),
            tool_id: primary_tool_id.unwrap_or_default(),
            tool_call_id: primary_tool_call_id.unwrap_or_default(),
        })
        .await;

    Ok(total_tool_calls + call_count)
}
