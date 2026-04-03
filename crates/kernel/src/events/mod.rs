use async_trait::async_trait;
use llm::usage::Usage;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    RunStarted {
        session_id: String,
        thread_id: String,
        input: String,
    },
    ModelRequested {
        message_count: usize,
        tool_count: usize,
    },
    ToolCallRequested {
        name: String,
        arguments: serde_json::Value,
    },
    ToolCallCompleted {
        name: String,
        output: String,
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
