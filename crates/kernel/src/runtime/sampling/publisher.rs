use crate::{
    events::{AgentEvent, EventSink},
    model::ResponseEvent,
};

use super::StreamSideEffect;

/// Publishes stream-derived queue side effects that become visible before final stream completion.
pub(super) async fn publish_stream_side_effects<E>(
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

/// Publishes one streamed response event into the runtime sink without waiting for final aggregation.
pub(super) async fn publish_response_event<E>(events: &E, event: &ResponseEvent, iteration: usize)
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
