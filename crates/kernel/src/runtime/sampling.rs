use futures_util::StreamExt;
use llm::usage::Usage;

use super::inflight::{CompletedToolCallQueue, InFlightToolCallRegistry};
use super::tool_runtime::ToolExecutionPlan;
use crate::{
    Result,
    events::ToolCallInFlightState,
    events::{AgentEvent, EventSink},
    model::{ResponseEvent, ResponseItem},
};

/// Captures the stream result for a single iteration without reconstructing a generic model response.
#[derive(Debug, Clone)]
pub(crate) struct StreamIterationResult {
    pub(crate) text: Option<String>,
    pub(crate) ready_tool_calls: CompletedToolCallQueue,
    pub(crate) in_flight_tool_calls: InFlightToolCallRegistry,
    pub(crate) usage: Usage,
    pub(crate) message_id: Option<String>,
}

/// Describes how the loop should proceed after one model stream finishes.
#[derive(Debug, Clone)]
pub(crate) enum IterationOutcome {
    Respond {
        message_id: Option<String>,
        text: String,
    },
    ContinueWithTools(ToolExecutionPlan),
}

/// Side effects emitted while converting streamed response items into runtime queue state.
#[derive(Debug, Clone)]
enum StreamSideEffect {
    Queued {
        name: String,
        tool_id: String,
        tool_call_id: String,
    },
    Registered {
        name: String,
        tool_id: String,
        tool_call_id: String,
        handle_id: String,
    },
    StateUpdated {
        name: String,
        tool_id: String,
        tool_call_id: String,
        handle_id: String,
        state: ToolCallInFlightState,
        error_summary: Option<String>,
    },
}

/// Converts one completed stream aggregation into the next loop action.
impl From<StreamIterationResult> for IterationOutcome {
    fn from(iteration_result: StreamIterationResult) -> Self {
        if iteration_result.ready_tool_calls.is_empty() {
            Self::Respond {
                message_id: iteration_result.message_id,
                text: iteration_result.text.unwrap_or_default(),
            }
        } else {
            Self::ContinueWithTools(ToolExecutionPlan {
                message_id: iteration_result.message_id,
                text: iteration_result.text,
                queue: iteration_result.ready_tool_calls,
                in_flight: iteration_result.in_flight_tool_calls,
            })
        }
    }
}

/// Aggregates the stream output for one iteration directly from response events.
#[derive(Debug, Default)]
struct StreamResponseCollector {
    streamed_text: String,
    completed_message_text: Option<String>,
    saw_text_delta: bool,
    ready_tool_calls: CompletedToolCallQueue,
    in_flight_tool_calls: InFlightToolCallRegistry,
    usage: Usage,
    message_id: Option<String>,
    completed: bool,
}

impl StreamResponseCollector {
    /// Builds an iteration collector whose handle allocation continues from the current turn.
    fn new(next_tool_handle_sequence: usize) -> Self {
        Self {
            streamed_text: String::new(),
            completed_message_text: None,
            saw_text_delta: false,
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
    fn record_event(&mut self, event: &ResponseEvent) -> Vec<StreamSideEffect> {
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
                self.ready_tool_calls
                    .push_completed(crate::tools::ToolCallRequest {
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
            })
            | ResponseEvent::OutputItemDone(ResponseItem::Reasoning { .. }) => Vec::new(),
        }
    }

    /// Converts the collected stream state into the loop's iteration-specific result.
    #[allow(clippy::result_large_err)]
    fn build(self) -> Result<StreamIterationResult> {
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
            ready_tool_calls: self.ready_tool_calls,
            in_flight_tool_calls: self.in_flight_tool_calls,
            usage: self.usage,
            message_id: self.message_id,
        })
    }
}

/// Consumes one model stream while publishing runtime events and collecting the resulting output.
pub(crate) async fn collect_stream_response<E>(
    events: &E,
    iteration: usize,
    next_tool_handle_sequence: usize,
    stream: &mut crate::model::ResponseEventStream,
) -> Result<StreamIterationResult>
where
    E: EventSink,
{
    let mut collector = StreamResponseCollector::new(next_tool_handle_sequence);

    while let Some(event) = stream.next().await {
        let event = event?;
        publish_response_event(events, &event, iteration).await;
        let side_effects = collector.record_event(&event);
        publish_stream_side_effects(events, side_effects, iteration).await;
    }

    collector.build()
}

/// Publishes stream-derived queue side effects that become visible before the final stream completion.
async fn publish_stream_side_effects<E>(
    events: &E,
    side_effects: Vec<StreamSideEffect>,
    iteration: usize,
) where
    E: EventSink,
{
    for side_effect in side_effects {
        match side_effect {
            StreamSideEffect::Queued {
                name,
                tool_id,
                tool_call_id,
            } => {
                events
                    .publish(AgentEvent::ToolCallQueued {
                        name,
                        iteration: Some(iteration),
                        tool_id,
                        tool_call_id,
                    })
                    .await;
            }
            StreamSideEffect::Registered {
                name,
                tool_id,
                tool_call_id,
                handle_id,
            } => {
                events
                    .publish(AgentEvent::ToolCallInFlightRegistered {
                        name,
                        iteration: Some(iteration),
                        tool_id,
                        tool_call_id,
                        handle_id,
                    })
                    .await;
            }
            StreamSideEffect::StateUpdated {
                name,
                tool_id,
                tool_call_id,
                handle_id,
                state,
                error_summary,
            } => {
                events
                    .publish(AgentEvent::ToolCallInFlightStateUpdated {
                        name,
                        iteration: Some(iteration),
                        tool_id,
                        tool_call_id,
                        handle_id,
                        state,
                        error_summary,
                    })
                    .await;
            }
        }
    }
}

/// Publishes one streamed response event into the runtime event sink without waiting for final aggregation.
async fn publish_response_event<E>(events: &E, event: &ResponseEvent, iteration: usize)
where
    E: EventSink,
{
    match event {
        ResponseEvent::Created => {
            events
                .publish(AgentEvent::ModelResponseCreated {
                    iteration: Some(iteration),
                })
                .await;
        }
        ResponseEvent::OutputTextDelta(text) => {
            events
                .publish(AgentEvent::ModelTextDelta {
                    text: text.clone(),
                    iteration: Some(iteration),
                })
                .await;
        }
        ResponseEvent::ReasoningSummaryDelta {
            id,
            delta,
            summary_index,
        } => {
            events
                .publish(AgentEvent::ModelReasoningSummaryDelta {
                    id: id.clone(),
                    text: delta.clone(),
                    summary_index: *summary_index,
                    iteration: Some(iteration),
                })
                .await;
        }
        ResponseEvent::ReasoningContentDelta {
            id,
            delta,
            content_index,
        } => {
            events
                .publish(AgentEvent::ModelReasoningContentDelta {
                    id: id.clone(),
                    text: delta.clone(),
                    content_index: *content_index,
                    iteration: Some(iteration),
                })
                .await;
        }
        ResponseEvent::ToolCallNameDelta {
            item_id,
            call_id,
            delta,
        } => {
            events
                .publish(AgentEvent::ModelToolCallNameDelta {
                    tool_id: item_id.clone(),
                    tool_call_id: call_id.clone(),
                    delta: delta.clone(),
                    iteration: Some(iteration),
                })
                .await;
        }
        ResponseEvent::ToolCallArgumentsDelta {
            item_id,
            call_id,
            delta,
        } => {
            events
                .publish(AgentEvent::ModelToolCallArgumentsDelta {
                    tool_id: item_id.clone(),
                    tool_call_id: call_id.clone(),
                    delta: delta.clone(),
                    iteration: Some(iteration),
                })
                .await;
        }
        ResponseEvent::OutputItemAdded(item) => {
            events
                .publish(AgentEvent::ModelOutputItemAdded {
                    item: item.clone(),
                    iteration: Some(iteration),
                })
                .await;
        }
        ResponseEvent::OutputItemUpdated(item) => {
            events
                .publish(AgentEvent::ModelOutputItemUpdated {
                    item: item.clone(),
                    iteration: Some(iteration),
                })
                .await;
        }
        ResponseEvent::OutputItemDone(item) => {
            events
                .publish(AgentEvent::ModelOutputItemDone {
                    item: item.clone(),
                    iteration: Some(iteration),
                })
                .await;
        }
        ResponseEvent::Completed { usage, message_id } => {
            events
                .publish(AgentEvent::ModelStreamCompleted {
                    message_id: message_id.clone(),
                    usage: *usage,
                    iteration: Some(iteration),
                })
                .await;
        }
    }
}
