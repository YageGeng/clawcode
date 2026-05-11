//! Turn execution — processes a single user prompt through the LLM.

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc;

use protocol::message::{AssistantContent, Message};
use protocol::one_or_many::OneOrMany;
use protocol::{AgentPath, Event, KernelError, SessionId, ToolCallStatus};
use provider::completion::request::CompletionRequest;
use provider::factory::{ArcLlm, LlmStreamEvent};

use crate::context::ContextManager;
use crate::tool::ToolRegistry;

/// Immutable snapshot of all context needed to execute a single turn.
///
/// Uses `typed-builder` per project convention (more than 3 fields).
#[derive(Clone, typed_builder::TypedBuilder)]
pub(crate) struct TurnContext {
    /// The session this turn belongs to.
    pub session_id: SessionId,
    /// Shared LLM handle for streaming completion requests.
    pub llm: ArcLlm,
    /// Shared tool registry for resolving and executing tool calls.
    pub tools: Arc<ToolRegistry>,
    /// Working directory for the session.
    pub cwd: PathBuf,
}

/// Execute a single turn: build the request, call the LLM, emit events,
/// execute tool calls inline, and save the assistant message to context.
///
/// # Errors
///
/// Returns `KernelError::Internal` if the LLM stream fails.
pub(crate) async fn execute_turn(
    ctx: &TurnContext,
    user_text: String,
    context: &mut Box<dyn ContextManager>,
    tx_event: &mpsc::UnboundedSender<Event>,
) -> Result<(), KernelError> {
    // 1. Build user message and push to context
    let user_msg = Message::user(user_text);
    context.push(user_msg.clone());

    // 2. Build CompletionRequest
    let history = context.history();
    let history = OneOrMany::many(history).map_err(|e| KernelError::Internal(e.into()))?;

    let tool_defs: Vec<provider::completion::request::ToolDefinition> = ctx
        .tools
        .definitions()
        .into_iter()
        .map(|d| provider::completion::request::ToolDefinition {
            name: d.name,
            description: d.description,
            parameters: d.parameters,
        })
        .collect();

    let request = CompletionRequest::builder()
        .model(Some(ctx.llm.model_id().to_string()))
        .chat_history(history)
        .tools(tool_defs)
        .build();

    // 3. Call LLM streaming API
    let mut stream = ctx
        .llm
        .stream(request)
        .await
        .map_err(|e| KernelError::Internal(anyhow::anyhow!("LLM stream error: {e}")))?;

    // 4. Consume stream, translate events, execute tools inline
    let mut assistant_content: Vec<AssistantContent> = Vec::new();

    while let Some(event) = stream.next().await {
        let event =
            event.map_err(|e| KernelError::Internal(anyhow::anyhow!("Stream event error: {e}")))?;

        match event {
            LlmStreamEvent::Text(text) => {
                assistant_content.push(AssistantContent::Text(text.clone()));
                let ev = Event::AgentMessageChunk {
                    session_id: ctx.session_id.clone(),
                    text: text.text,
                };
                let _ = tx_event.send(ev);
            }
            LlmStreamEvent::ToolCall {
                tool_call,
                internal_call_id,
            } => {
                // Emit InProgress event
                let _ = tx_event.send(Event::ToolCall {
                    session_id: ctx.session_id.clone(),
                    agent_path: AgentPath::root(),
                    call_id: internal_call_id.clone(),
                    name: tool_call.function.name.clone(),
                    arguments: tool_call.function.arguments.clone(),
                    status: ToolCallStatus::InProgress,
                });

                // Execute the tool
                let output = ctx
                    .tools
                    .execute(
                        &tool_call.function.name,
                        tool_call.function.arguments.clone(),
                        &ctx.cwd,
                    )
                    .await;

                match output {
                    Ok(out) => {
                        assistant_content.push(AssistantContent::ToolCall(tool_call));
                        let _ = tx_event.send(Event::ToolCallUpdate {
                            session_id: ctx.session_id.clone(),
                            call_id: internal_call_id,
                            output_delta: Some(out),
                            status: Some(ToolCallStatus::Completed),
                        });
                    }
                    Err(err) => {
                        let _ = tx_event.send(Event::ToolCallUpdate {
                            session_id: ctx.session_id.clone(),
                            call_id: internal_call_id,
                            output_delta: Some(err),
                            status: Some(ToolCallStatus::Failed),
                        });
                    }
                }
            }
            LlmStreamEvent::Reasoning(reasoning) => {
                let _ = tx_event.send(Event::AgentThoughtChunk {
                    session_id: ctx.session_id.clone(),
                    text: reasoning.display_text(),
                });
            }
            LlmStreamEvent::ReasoningDelta { reasoning, .. } => {
                let _ = tx_event.send(Event::AgentThoughtChunk {
                    session_id: ctx.session_id.clone(),
                    text: reasoning,
                });
            }
            LlmStreamEvent::Final {
                usage: Some(usage), ..
            } => {
                let _ = tx_event.send(Event::UsageUpdate {
                    session_id: ctx.session_id.clone(),
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                });
            }
            _ => {}
        }
    }

    // 5. Save assistant message to context
    if !assistant_content.is_empty() {
        let assistant_msg = Message::Assistant {
            id: None,
            content: OneOrMany::many(assistant_content)
                .unwrap_or_else(|_| OneOrMany::one(AssistantContent::text(""))),
        };
        context.push(assistant_msg);
    }

    Ok(())
}
