use llm::{
    completion::message::{Reasoning, ReasoningContent},
    usage::Usage,
};

use crate::{
    Result,
    events::ToolCallInFlightState,
    model::{ResponseEvent, ResponseItem},
    runtime::inflight::{CompletedToolCallQueue, InFlightToolCallRegistry},
    tools::ToolCallRequest,
};

use super::{StreamIterationResult, StreamSideEffect};

/// Aggregates one streamed model iteration into the runtime-specific result shape.
#[derive(Debug, Default)]
pub(super) struct StreamResponseCollector {
    streamed_text: String,
    completed_message_text: Option<String>,
    saw_text_delta: bool,
    reasoning_items: Vec<Reasoning>,
    ready_tool_calls: CompletedToolCallQueue,
    in_flight_tool_calls: InFlightToolCallRegistry,
    usage: Usage,
    message_id: Option<String>,
    completed: bool,
}

impl StreamResponseCollector {
    /// Builds an iteration collector whose handle allocation continues from the current turn.
    pub(super) fn new(next_tool_handle_sequence: usize) -> Self {
        Self {
            streamed_text: String::new(),
            completed_message_text: None,
            saw_text_delta: false,
            reasoning_items: Vec::new(),
            ready_tool_calls: CompletedToolCallQueue::default(),
            in_flight_tool_calls: InFlightToolCallRegistry::with_next_handle_sequence(
                next_tool_handle_sequence,
            ),
            usage: Usage::new(),
            message_id: None,
            completed: false,
        }
    }

    /// Applies one streamed event to the current iteration's aggregate output state.
    pub(super) fn record_event(&mut self, event: &ResponseEvent) -> Vec<StreamSideEffect> {
        match event {
            ResponseEvent::OutputTextDelta(text) => {
                self.saw_text_delta = true;
                self.streamed_text.push_str(text);
                Vec::new()
            }
            ResponseEvent::OutputItemDone(ResponseItem::ToolCall {
                item_id,
                call_id,
                name,
                arguments: Some(arguments),
                ..
            }) => {
                let tool_call_id = call_id.clone().unwrap_or_else(|| item_id.clone());
                self.ready_tool_calls.push_completed(ToolCallRequest {
                    id: item_id.clone(),
                    call_id: call_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
                let registered_entry = self.in_flight_tool_calls.register(
                    name.clone(),
                    item_id.clone(),
                    tool_call_id.clone(),
                    ToolCallInFlightState::Queued,
                );
                vec![
                    StreamSideEffect::Queued {
                        name: name.clone(),
                        tool_id: item_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                    },
                    StreamSideEffect::Registered {
                        name: registered_entry.name.clone(),
                        tool_id: registered_entry.tool_id.clone(),
                        tool_call_id: registered_entry.tool_call_id.clone(),
                        handle_id: registered_entry.handle_id.clone(),
                    },
                    StreamSideEffect::StateUpdated {
                        name: registered_entry.name,
                        tool_id: registered_entry.tool_id,
                        tool_call_id: registered_entry.tool_call_id,
                        handle_id: registered_entry.handle_id,
                        state: ToolCallInFlightState::Queued,
                        error_summary: None,
                    },
                ]
            }
            ResponseEvent::OutputItemDone(ResponseItem::Message { text }) => {
                if !self.saw_text_delta {
                    self.completed_message_text = Some(text.clone());
                }
                Vec::new()
            }
            ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
                id,
                summary,
                content,
                encrypted_content,
            }) => {
                let mut reasoning = Reasoning::summaries(summary.clone()).optional_id(id.clone());
                if let Some(encrypted_content) = encrypted_content.clone() {
                    reasoning
                        .content
                        .push(ReasoningContent::Encrypted(encrypted_content));
                }
                reasoning
                    .content
                    .extend(content.iter().cloned().map(|text| ReasoningContent::Text {
                        text,
                        signature: None,
                    }));
                self.reasoning_items.push(reasoning);
                Vec::new()
            }
            ResponseEvent::Completed { usage, message_id } => {
                self.usage = *usage;
                self.message_id = message_id.clone();
                self.completed = true;
                Vec::new()
            }
            ResponseEvent::Created
            | ResponseEvent::OutputItemAdded(_)
            | ResponseEvent::OutputItemUpdated(_)
            | ResponseEvent::ToolCallNameDelta { .. }
            | ResponseEvent::ToolCallArgumentsDelta { .. }
            | ResponseEvent::ReasoningSummaryDelta { .. }
            | ResponseEvent::ReasoningContentDelta { .. }
            | ResponseEvent::OutputItemDone(ResponseItem::ToolCall {
                arguments: None, ..
            }) => Vec::new(),
        }
    }

    /// Converts the collected stream state into the loop's iteration-specific result.
    #[allow(clippy::result_large_err)]
    pub(super) fn build(self) -> Result<StreamIterationResult> {
        if !self.completed {
            return Err(crate::Error::Runtime {
                message: "model stream closed before stream-completed event".to_string(),
                stage: "agent-loop-stream-completed".to_string(),
                inflight_snapshot: None,
            });
        }

        let text = if self.streamed_text.is_empty() {
            self.completed_message_text.filter(|text| !text.is_empty())
        } else {
            Some(self.streamed_text)
        };

        Ok(StreamIterationResult {
            text,
            reasoning: self.reasoning_items,
            ready_tool_calls: self.ready_tool_calls,
            in_flight_tool_calls: self.in_flight_tool_calls,
            usage: self.usage,
            message_id: self.message_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::StreamResponseCollector;
    use crate::model::{ResponseEvent, ResponseItem};

    /// Verifies stream aggregation preserves reasoning blocks for later tool continuations.
    #[test]
    fn collector_keeps_reasoning_items() {
        let mut collector = StreamResponseCollector::new(0);
        collector.record_event(&ResponseEvent::OutputItemDone(ResponseItem::Reasoning {
            id: Some("rs_1".to_string()),
            summary: vec!["plan".to_string()],
            content: vec!["hidden".to_string()],
            encrypted_content: Some("opaque".to_string()),
        }));
        collector.record_event(&ResponseEvent::Completed {
            message_id: Some("msg_1".to_string()),
            usage: llm::usage::Usage::new(),
        });

        let result = collector.build().expect("collector should build");
        assert_eq!(result.reasoning.len(), 1);
        assert_eq!(result.reasoning[0].id.as_deref(), Some("rs_1"));
        assert_eq!(result.reasoning[0].display_text(), "plan\nhidden");
        assert_eq!(result.reasoning[0].encrypted_content(), Some("opaque"));
    }
}
