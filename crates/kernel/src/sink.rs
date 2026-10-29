use async_trait::async_trait;
use protocol::{AgentEvent, SessionId};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Failures returned by a live protocol event consumer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SinkError {
    /// The protocol consumer stopped accepting events.
    #[error("event consumer closed")]
    Closed,
    /// A custom event consumer failed.
    #[error("event consumer failed: {0}")]
    Consumer(String),
}

/// Live event destination used by ACP and other protocol adapters.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Publishes one fully correlated event without changing kernel state.
    async fn emit(&self, event: AgentEvent) -> Result<(), SinkError>;
}

/// Routes Kernel-initiated runs to their Session's live projection.
pub trait SessionEventSinks: Send + Sync {
    /// Returns the event destination for one Session.
    fn sink(&self, session_id: &SessionId) -> Arc<dyn EventSink>;
}

/// Accepts internal events when no protocol adapter is installed.
pub(crate) struct DiscardEvents;

#[async_trait]
impl EventSink for DiscardEvents {
    /// Keeps headless Kernel runs independent of an external subscriber.
    async fn emit(&self, _event: AgentEvent) -> Result<(), SinkError> {
        Ok(())
    }
}

/// Event sink backed by a bounded Tokio channel.
pub struct ChannelEventSink {
    sender: mpsc::Sender<AgentEvent>,
}

impl ChannelEventSink {
    /// Creates a sink that forwards events to the supplied bounded channel.
    #[must_use]
    pub fn new(sender: mpsc::Sender<AgentEvent>) -> Self {
        Self { sender }
    }
}

#[async_trait]
impl EventSink for ChannelEventSink {
    /// Sends one event or reports that the receiving protocol session closed.
    async fn emit(&self, event: AgentEvent) -> Result<(), SinkError> {
        self.sender
            .send(event)
            .await
            .map_err(|_send_error| SinkError::Closed)
    }
}
