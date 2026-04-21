use async_trait::async_trait;
use llm::usage::Usage;
use tokio::sync::Mutex;

use crate::model::ResponseItem;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStage {
    ModelRequesting,
    Responding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStage {
    Calling,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallInFlightState {
    Queued,
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskContinuationAction {
    Continue,
    Finish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskContinuationSource {
    PendingInput,
    SystemFollowUp,
    TaskCompleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskContinuationDecisionStage {
    ToolBatchCompletedHook,
    BeforeFinalResponseHook,
    TurnCompletedHook,
    Resolver,
    SessionQueue,
    FinalDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskContinuationDecisionKind {
    Continue,
    Request,
    Replace,
    Adopted,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskContinuationDecisionTraceEntry {
    pub stage: TaskContinuationDecisionStage,
    pub decision: TaskContinuationDecisionKind,
    pub source: Option<TaskContinuationSource>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    RunStarted {
        session_id: String,
        thread_id: String,
        input: String,
    },
    StatusUpdated {
        stage: AgentStage,
        message: Option<String>,
        iteration: Option<usize>,
        tool_id: Option<String>,
        tool_call_id: Option<String>,
    },
    ToolStatusUpdated {
        stage: ToolStage,
        name: String,
        iteration: Option<usize>,
        tool_id: String,
        tool_call_id: String,
    },
    ModelRequested {
        message_count: usize,
        tool_count: usize,
    },
    ModelResponseCreated {
        iteration: Option<usize>,
    },
    ModelTextDelta {
        text: String,
        iteration: Option<usize>,
    },
    ModelReasoningSummaryDelta {
        id: Option<String>,
        text: String,
        summary_index: i64,
        iteration: Option<usize>,
    },
    ModelReasoningContentDelta {
        id: Option<String>,
        text: String,
        content_index: i64,
        iteration: Option<usize>,
    },
    ModelToolCallNameDelta {
        tool_id: String,
        tool_call_id: Option<String>,
        delta: String,
        iteration: Option<usize>,
    },
    ModelToolCallArgumentsDelta {
        tool_id: String,
        tool_call_id: Option<String>,
        delta: String,
        iteration: Option<usize>,
    },
    ModelOutputItemAdded {
        item: ResponseItem,
        iteration: Option<usize>,
    },
    ModelOutputItemUpdated {
        item: ResponseItem,
        iteration: Option<usize>,
    },
    ModelOutputItemDone {
        item: ResponseItem,
        iteration: Option<usize>,
    },
    ModelStreamCompleted {
        message_id: Option<String>,
        usage: Usage,
        iteration: Option<usize>,
    },
    ToolCallQueued {
        name: String,
        iteration: Option<usize>,
        tool_id: String,
        tool_call_id: String,
    },
    ToolCallInFlightRegistered {
        name: String,
        iteration: Option<usize>,
        tool_id: String,
        tool_call_id: String,
        handle_id: String,
    },
    ToolCallInFlightStateUpdated {
        name: String,
        iteration: Option<usize>,
        tool_id: String,
        tool_call_id: String,
        handle_id: String,
        state: ToolCallInFlightState,
        error_summary: Option<String>,
    },
    ToolCallInFlightSnapshot {
        iteration: Option<usize>,
        queued_handles: Vec<String>,
        running_handles: Vec<String>,
        completed_handles: Vec<String>,
        cancelled_handles: Vec<String>,
        failed_handles: Vec<String>,
    },
    ToolCallRequested {
        name: String,
        handle_id: String,
        arguments: serde_json::Value,
    },
    ToolCallCompleted {
        name: String,
        handle_id: String,
        output: String,
        structured_output: Option<serde_json::Value>,
    },
    TaskContinuationDecided {
        turn_index: usize,
        action: TaskContinuationAction,
        source: TaskContinuationSource,
        decision_trace: Vec<TaskContinuationDecisionTraceEntry>,
    },
    TextProduced {
        text: String,
    },
    RunFinished {
        text: String,
        usage: Usage,
    },
}

#[async_trait]
pub trait EventSink: Send + Sync {
    /// Publishes one runtime event to the configured sink implementation.
    async fn publish(&self, event: AgentEvent);
}

#[derive(Debug, Default)]
pub struct NoopEventSink;

#[async_trait]
impl EventSink for NoopEventSink {
    /// Discards every runtime event.
    async fn publish(&self, _event: AgentEvent) {}
}

#[derive(Debug, Default)]
pub struct RecordingEventSink {
    events: Mutex<Vec<AgentEvent>>,
}

impl RecordingEventSink {
    /// Returns a snapshot of all captured events for assertions.
    pub async fn snapshot(&self) -> Vec<AgentEvent> {
        self.events.lock().await.clone()
    }
}

#[async_trait]
impl EventSink for RecordingEventSink {
    /// Records each runtime event in memory for tests and debugging.
    async fn publish(&self, event: AgentEvent) {
        self.events.lock().await.push(event);
    }
}
