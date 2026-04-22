use llm::{completion::Message, usage::Usage};

use crate::{
    Result,
    context::SessionTaskContext,
    events::{AgentEvent, EventSink, TaskContinuationDecisionTraceEntry},
    runtime::{inflight::ToolCallRuntimeSnapshot, turn::LoopResult},
    session::{SessionContinuationRequest, SessionId, ThreadId},
};

/// Input required to persist the final assistant text and package one completed loop result.
#[derive(Debug, Clone)]
pub(crate) struct FinalizeTextResponseRequest {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub message_id: Option<String>,
    pub text: String,
    pub usage: Usage,
    pub new_messages: Vec<Message>,
    pub iteration: usize,
    pub inflight_snapshot: ToolCallRuntimeSnapshot,
    pub requested_continuation: Option<SessionContinuationRequest>,
    pub continuation_decision_trace: Vec<TaskContinuationDecisionTraceEntry>,
    pub next_tool_handle_sequence: usize,
}

/// Persists the final assistant message, publishes the terminal text event, and returns one loop result.
pub(crate) async fn finalize_text_response<E>(
    store: &SessionTaskContext,
    events: &E,
    request: FinalizeTextResponseRequest,
) -> Result<LoopResult>
where
    E: EventSink,
{
    let FinalizeTextResponseRequest {
        session_id,
        thread_id,
        message_id,
        text,
        usage,
        mut new_messages,
        iteration,
        inflight_snapshot,
        requested_continuation,
        continuation_decision_trace,
        next_tool_handle_sequence,
    } = request;

    events
        .publish(AgentEvent::TextProduced { text: text.clone() })
        .await;
    let assistant = message_id
        .map(|id| Message::assistant_with_id(id, text.clone()))
        .unwrap_or_else(|| Message::assistant(text.clone()));
    store
        .append_message_state(session_id, thread_id, assistant.clone())
        .await?;
    new_messages.push(assistant);

    Ok(LoopResult {
        final_text: text,
        usage,
        new_messages,
        iterations: iteration,
        inflight_snapshot,
        requested_continuation,
        continuation_decision_trace,
        next_tool_handle_sequence,
    })
}

#[cfg(test)]
mod tests {
    use llm::{completion::Message, usage::Usage};

    use super::{FinalizeTextResponseRequest, finalize_text_response};
    use crate::{
        events::{AgentEvent, RecordingEventSink},
        session::{InMemorySessionStore, SessionId, ThreadId},
    };

    /// Verifies the final text response finalizer persists the assistant message and reports text output.
    #[tokio::test]
    async fn finalize_text_response_persists_assistant_message_and_returns_loop_result() {
        let store = InMemorySessionStore::default();
        let session_id = SessionId::new();
        let thread_id = ThreadId::new();
        let events = RecordingEventSink::default();
        let user_message = Message::user("hello");
        let usage = Usage {
            input_tokens: 3,
            output_tokens: 5,
            total_tokens: 8,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };

        store
            .begin_turn_state(
                session_id.clone(),
                thread_id.clone(),
                "hello".to_string(),
                user_message.clone(),
            )
            .await
            .expect("test should be able to seed an active turn");

        let result = finalize_text_response(
            &store,
            &events,
            FinalizeTextResponseRequest {
                session_id: session_id.clone(),
                thread_id: thread_id.clone(),
                message_id: Some("msg_123".to_string()),
                text: "hello from agent".to_string(),
                usage,
                new_messages: Vec::new(),
                iteration: 2,
                inflight_snapshot: Default::default(),
                requested_continuation: None,
                continuation_decision_trace: Vec::new(),
                next_tool_handle_sequence: 7,
            },
        )
        .await
        .expect("finalizer should succeed");

        let messages = store
            .load_messages_state(session_id, thread_id, 10)
            .await
            .expect("messages should be readable");
        let recorded_events = events.snapshot().await;

        assert_eq!(result.final_text, "hello from agent");
        assert_eq!(result.iterations, 2);
        assert_eq!(result.next_tool_handle_sequence, 7);
        assert_eq!(
            messages,
            vec![
                user_message,
                Message::assistant_with_id("msg_123".to_string(), "hello from agent"),
            ]
        );
        assert!(matches!(
            recorded_events.as_slice(),
            [AgentEvent::TextProduced { text }] if text == "hello from agent"
        ));
    }
}
