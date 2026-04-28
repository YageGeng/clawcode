mod collector;
mod publisher;

use futures_util::StreamExt;
use llm::completion::message::Reasoning;
use llm::usage::Usage;

use self::{
    collector::StreamResponseCollector,
    publisher::{publish_response_event, publish_stream_side_effects},
};
use super::tool::ToolExecutionPlan;
use crate::{
    Result,
    events::EventSink,
    events::ToolCallInFlightState,
    runtime::inflight::{CompletedToolCallQueue, InFlightToolCallRegistry},
};

/// Captures the stream result for a single iteration without reconstructing a generic model response.
#[derive(Debug, Clone)]
pub(crate) struct StreamIterationResult {
    pub(crate) text: Option<String>,
    pub(crate) reasoning: Vec<Reasoning>,
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
        reasoning: Vec<Reasoning>,
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
                reasoning: iteration_result.reasoning,
            }
        } else {
            Self::ContinueWithTools(ToolExecutionPlan {
                message_id: iteration_result.message_id,
                text: iteration_result.text,
                reasoning: iteration_result.reasoning,
                queue: iteration_result.ready_tool_calls,
                in_flight: iteration_result.in_flight_tool_calls,
            })
        }
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
