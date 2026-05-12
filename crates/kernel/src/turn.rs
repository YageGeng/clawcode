//! Turn execution — processes a single user prompt through the LLM
//! with multi-turn tool loop support.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use protocol::message::{AssistantContent, Message, ToolResult, ToolResultContent};
use protocol::one_or_many::OneOrMany;
use protocol::{
    AgentPath, Event, KernelError, ReviewDecision, SessionId, ToolCallDeltaContent, ToolCallStatus,
};
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
    /// Pending approval channels. execute_turn inserts a oneshot sender;
    /// the session background task resolves it when the user responds.
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
    context.push(Message::user(user_text));

    let tool_defs = ctx.tools.definitions();
    let sid = &ctx.session_id;

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
                    assistant_content.push(AssistantContent::text(&text.text));
                    let _ = tx_event.send(Event::message_chunk(sid.clone(), text.text));
                }
                LlmStreamEvent::ToolCall {
                    mut tool_call,
                    internal_call_id,
                } => {
                    if tool_call.id.is_empty() {
                        tool_call.id = internal_call_id.clone();
                    }

                    let _ = tx_event.send(Event::tool_call(
                        sid.clone(),
                        AgentPath::root(),
                        internal_call_id.clone(),
                        tool_call.function.name.clone(),
                        tool_call.function.arguments.clone(),
                        ToolCallStatus::Pending,
                    ));

                    let needs_approval = ctx
                        .tools
                        .get(&tool_call.function.name)
                        .is_some_and(|tool| tool.needs_approval(&tool_call.function.arguments));

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
                            ReviewDecision::Abort => return Err(KernelError::Cancelled),
                            _ => Err("rejected by user".to_string()),
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

                    let succeeded = output.is_ok();

                    let text = output.as_ref().map_or_else(|e| e.clone(), Clone::clone);

                    assistant_content.push(AssistantContent::ToolCall(tool_call.clone()));
                    tool_outputs.push(ToolOutput {
                        id: tool_call.id.clone(),
                        call_id: tool_call.call_id.clone(),
                        output: text.clone(),
                    });
                    let _ = tx_event.send(Event::tool_call_update(
                        sid.clone(),
                        internal_call_id,
                        Some(text),
                        Some(if succeeded {
                            ToolCallStatus::Completed
                        } else {
                            ToolCallStatus::Failed
                        }),
                    ));
                }
                LlmStreamEvent::Reasoning(reasoning) => {
                    let text = reasoning.display_text();
                    assistant_content.push(AssistantContent::Reasoning(reasoning));
                    let _ = tx_event.send(Event::thought_chunk(sid.clone(), text));
                }
                LlmStreamEvent::ReasoningDelta { reasoning, .. } => {
                    reasoning_text.push_str(&reasoning);
                    let _ = tx_event.send(Event::thought_chunk(sid.clone(), reasoning));
                }
                LlmStreamEvent::ToolCallDelta {
                    internal_call_id,
                    content,
                    ..
                } => {
                    let content = match content {
                        provider::streaming::ToolCallDeltaContent::Name(name) => {
                            ToolCallDeltaContent::name(name)
                        }
                        provider::streaming::ToolCallDeltaContent::Delta(delta) => {
                            ToolCallDeltaContent::delta(delta)
                        }
                    };

                    // Forward incremental tool call arguments to the frontend.
                    let _ = tx_event.send(Event::tool_call_delta(
                        sid.clone(),
                        internal_call_id,
                        content,
                    ));
                }
                LlmStreamEvent::Final {
                    usage: Some(usage), ..
                } => {
                    let _ = tx_event.send(Event::usage_update(
                        sid.clone(),
                        usage.input_tokens,
                        usage.output_tokens,
                    ));
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
/// Blocks on a oneshot channel until the session background task
/// receives the user's response.
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

    let event = Event::exec_approval(
        ctx.session_id.clone(),
        call_id.to_string(),
        tool_name.to_string(),
        arguments,
        ctx.cwd.clone(),
    );

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
