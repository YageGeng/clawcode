use llm::completion::{Message, message::Reasoning};
use tokio_util::sync::CancellationToken;

use crate::{
    context::SessionTaskContext,
    runtime::inflight::{CompletedToolCallQueue, InFlightToolCallRegistry},
    session::{SessionId, ThreadId},
    tools::{
        ToolContext,
        executor::{ToolExecutionMode, ToolExecutionRequest},
        router::ToolRouter,
    },
};

/// Queue of completed tool calls ready to execute after one stream finishes.
#[derive(Debug, Clone)]
pub(crate) struct ToolExecutionPlan {
    pub(crate) message_id: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) reasoning: Vec<Reasoning>,
    pub(crate) queue: CompletedToolCallQueue,
    pub(crate) in_flight: InFlightToolCallRegistry,
}

/// Runtime input needed to execute one tool batch and fold its messages back into the turn state.
pub(crate) struct ToolExecutionRuntimeInput<'a, E>
where
    E: crate::events::EventSink + ?Sized,
{
    pub(crate) store: &'a SessionTaskContext,
    pub(crate) session_id: SessionId,
    pub(crate) thread_id: ThreadId,
    pub(crate) router: &'a ToolRouter,
    pub(crate) events: &'a E,
    pub(crate) working_messages: &'a mut Vec<Message>,
    pub(crate) new_messages: &'a mut Vec<Message>,
}

/// One execution batch plus the batch-scoped runtime metadata needed to drain it.
#[derive(Debug, Clone)]
pub(super) struct ToolCallBatch {
    pub(super) message_id: Option<String>,
    pub(super) text: Option<String>,
    pub(super) reasoning: Vec<Reasoning>,
    pub(super) calls: Vec<ToolExecutionRequest>,
    pub(super) total_tool_calls: usize,
    pub(super) max_tool_calls: usize,
    pub(super) tool_execution_mode: ToolExecutionMode,
    pub(super) cancellation_token: Option<CancellationToken>,
    pub(super) tool_context: ToolContext,
}
