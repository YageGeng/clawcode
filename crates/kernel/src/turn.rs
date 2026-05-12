//! Turn execution — processes a single user prompt through the LLM
//! with multi-turn tool loop support.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use protocol::message::{AssistantContent, Message, ToolResult, ToolResultContent};
use protocol::one_or_many::OneOrMany;
use protocol::{AgentPath, Event, KernelError, ReviewDecision, SessionId, ToolCallStatus};
use provider::completion::request::CompletionRequest;
use provider::factory::{ArcLlm, LlmStreamEvent};

use crate::context::ContextManager;
use tools::ToolRegistry;

/// Immutable snapshot of all context needed to execute a single turn.
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
    /// Pending approval channels shared with the session background task.
    #[builder(default)]
    pub pending_approvals:
        Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<ReviewDecision>>>>,
}

/// Execute a single turn with multi-turn tool loop support:
/// - Push the user message to context
/// - Loop: call LLM → collect response → execute any tool calls → feed results back
/// - Exit when the LLM produces a response with no tool calls
pub(crate) async fn execute_turn(
    ctx: &TurnContext,
    user_text: String,
    context: &mut Box<dyn ContextManager>,
    tx_event: &mpsc::UnboundedSender<Event>,
) -> Result<(), KernelError> {
    // Push user message
    context.push(Message::user(user_text));

    let tool_defs = ctx.tools.definitions();

    // Multi-turn tool loop — runs until the LLM responds without tool calls
    loop {
        let history = context.history();
        let history = OneOrMany::many(history).map_err(|e| KernelError::Internal(e.into()))?;

        let request = CompletionRequest::builder()
            .model(Some(ctx.llm.model_id().to_string()))
            .chat_history(history)
            .tools(tool_defs.clone())
            .build();

        let mut stream = ctx
            .llm
            .stream(request)
            .await
            .map_err(|e| KernelError::Internal(anyhow::anyhow!("LLM stream error: {e}")))?;

        let mut assistant_content: Vec<AssistantContent> = Vec::new();
        let mut tool_outputs: Vec<ToolOutput> = Vec::new();
        let mut reasoning_text = String::new();

        while let Some(event) = stream.next().await {
            let event = event
                .map_err(|e| KernelError::Internal(anyhow::anyhow!("Stream event error: {e}")))?;

            match event {
                LlmStreamEvent::Text(text) => {
                    assistant_content.push(AssistantContent::Text(text.clone()));
                    let _ = tx_event.send(Event::AgentMessageChunk {
                        session_id: ctx.session_id.clone(),
                        text: text.text,
                    });
                }
                LlmStreamEvent::ToolCall {
                    mut tool_call,
                    internal_call_id,
                } => {
                    // DeepSeek/OpenAI-compatible streams may omit the provider id; use the
                    // internal id so assistant tool_calls and tool results still pair.
                    if tool_call.id.is_empty() {
                        tool_call.id = internal_call_id.clone();
                    }

                    let _ = tx_event.send(Event::ToolCall {
                        session_id: ctx.session_id.clone(),
                        agent_path: AgentPath::root(),
                        call_id: internal_call_id.clone(),
                        name: tool_call.function.name.clone(),
                        arguments: tool_call.function.arguments.clone(),
                        status: ToolCallStatus::Pending,
                    });

                    let needs_approval = ctx
                        .tools
                        .get(&tool_call.function.name)
                        .is_some_and(|t| t.needs_approval(&tool_call.function.arguments));

                    let output = if needs_approval {
                        match request_tool_approval(
                            ctx,
                            tx_event,
                            &internal_call_id,
                            &tool_call.function.name,
                            tool_call.function.arguments.clone(),
                        )
                        .await?
                        {
                            ReviewDecision::AllowOnce | ReviewDecision::AllowAlways => {
                                ctx.tools
                                    .execute(
                                        &tool_call.function.name,
                                        tool_call.function.arguments.clone(),
                                        &ctx.cwd,
                                    )
                                    .await
                            }
                            ReviewDecision::Abort => {
                                return Err(KernelError::Cancelled);
                            }
                            _ => {
                                // Rejected: still register the tool call + result
                                // so the API history is consistent
                                Err("rejected by user".to_string())
                            }
                        }
                    } else {
                        ctx.tools
                            .execute(
                                &tool_call.function.name,
                                tool_call.function.arguments.clone(),
                                &ctx.cwd,
                            )
                            .await
                    };

                    match output {
                        Ok(out) => {
                            assistant_content.push(AssistantContent::ToolCall(tool_call.clone()));
                            tool_outputs.push(ToolOutput {
                                id: tool_call.id.clone(),
                                call_id: tool_call.call_id.clone(),
                                output: out.clone(),
                            });
                            let _ = tx_event.send(Event::ToolCallUpdate {
                                session_id: ctx.session_id.clone(),
                                call_id: internal_call_id,
                                output_delta: Some(out),
                                status: Some(ToolCallStatus::Completed),
                            });
                        }
                        Err(err) => {
                            assistant_content.push(AssistantContent::ToolCall(tool_call.clone()));
                            tool_outputs.push(ToolOutput {
                                id: tool_call.id.clone(),
                                call_id: tool_call.call_id.clone(),
                                output: err.clone(),
                            });
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
                    let text = reasoning.display_text();
                    assistant_content.push(AssistantContent::Reasoning(reasoning));
                    let _ = tx_event.send(Event::AgentThoughtChunk {
                        session_id: ctx.session_id.clone(),
                        text,
                    });
                }
                LlmStreamEvent::ReasoningDelta { reasoning, .. } => {
                    reasoning_text.push_str(&reasoning);
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

        // Accumulate reasoning deltas into a Reasoning content item.
        // Skip if a complete Reasoning block was already added during stream consumption.
        let has_reasoning = assistant_content
            .iter()
            .any(|c| matches!(c, AssistantContent::Reasoning(_)));
        if !reasoning_text.is_empty() && !has_reasoning {
            assistant_content.push(AssistantContent::Reasoning(
                protocol::message::Reasoning::new(&reasoning_text),
            ));
        }

        // Save assistant message from this iteration
        if !assistant_content.is_empty() {
            context.push(Message::Assistant {
                id: None,
                content: OneOrMany::many(assistant_content)
                    .unwrap_or_else(|_| OneOrMany::one(AssistantContent::text(""))),
            });
        }

        // If no tool calls were made, the turn is done
        if tool_outputs.is_empty() {
            return Ok(());
        }

        // Feed tool outputs back as user messages for the next LLM iteration
        for output in tool_outputs {
            context.push(Message::User {
                content: OneOrMany::one(protocol::message::UserContent::ToolResult(ToolResult {
                    id: output.id,
                    call_id: output.call_id,
                    content: OneOrMany::one(ToolResultContent::Text(protocol::message::Text {
                        text: output.output,
                    })),
                })),
            });
        }
    }
}

/// Captures a tool execution result and the provider correlation id.
struct ToolOutput {
    /// Internal call id used by clawcode event streams.
    id: String,
    /// Provider call id required by APIs that separate local and remote ids.
    call_id: Option<String>,
    /// Text result sent back to the model.
    output: String,
}

/// Request approval for a tool call and return the frontend decision.
async fn request_tool_approval(
    ctx: &TurnContext,
    tx_event: &mpsc::UnboundedSender<Event>,
    call_id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<ReviewDecision, KernelError> {
    let (tx_approve, rx_approve) = oneshot::channel();
    {
        let mut approvals = ctx.pending_approvals.lock().await;
        approvals.insert(call_id.to_string(), tx_approve);
    }

    let event = Event::ExecApprovalRequested {
        session_id: ctx.session_id.clone(),
        call_id: call_id.to_string(),
        tool_name: tool_name.to_string(),
        arguments,
        cwd: ctx.cwd.clone(),
    };

    if tx_event.send(event).is_err() {
        ctx.pending_approvals.lock().await.remove(call_id);
        return Err(KernelError::Internal(anyhow::anyhow!(
            "failed to deliver approval request for tool call {call_id}"
        )));
    }

    rx_approve.await.map_err(|_| {
        KernelError::Internal(anyhow::anyhow!(
            "approval channel closed for tool call {call_id}"
        ))
    })
}
