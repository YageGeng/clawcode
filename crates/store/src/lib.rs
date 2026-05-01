mod error;
mod event;
mod reader;
mod writer;

pub use error::{Error, Result};
pub use event::{
    AgentRegistrationRecord, MailboxDeliveryRecord, PersistedAgentStatus,
    PersistedMailboxEventKind, SessionEvent, TurnEvent,
};
pub use reader::{
    SessionInfo, find_session_by_id, list_sessions, load_session_events, sessions_root,
};

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{Datelike, Utc};
use llm::{completion::Message, usage::Usage};
use uuid::Uuid;

use crate::writer::JsonlWriter;

/// Trait for session persistence backends.
///
/// Implementations receive turn lifecycle events and can persist them
/// to disk, a database, or any other durable store. All methods are
/// best-effort — the caller should treat failures as non-fatal.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Records one session event in durable storage.
    async fn record_event(&self, event: SessionEvent);

    /// Records the start of a new turn.
    async fn record_turn_started(
        &self,
        session_id: Uuid,
        thread_id: Uuid,
        user_text: &str,
        user_message: &Message,
    ) {
        self.record_event(SessionEvent::TurnStarted {
            session_id,
            thread_id,
            user_text: user_text.to_string(),
            message: user_message.clone(),
            timestamp: Utc::now(),
        })
        .await;
    }

    /// Records a message appended to the active turn.
    async fn record_message(&self, session_id: Uuid, thread_id: Uuid, message: &Message) {
        self.record_event(SessionEvent::Message {
            session_id,
            thread_id,
            message: message.clone(),
            timestamp: Utc::now(),
        })
        .await;
    }

    /// Records the completion of a turn with usage statistics and context snapshot.
    async fn record_turn_completed(
        &self,
        session_id: Uuid,
        thread_id: Uuid,
        usage: Usage,
        context_item: serde_json::Value,
    ) {
        self.record_event(SessionEvent::TurnCompleted {
            session_id,
            thread_id,
            usage,
            context_item,
            timestamp: Utc::now(),
        })
        .await;
    }

    /// Records that a turn was discarded before completion.
    async fn record_turn_discarded(&self, session_id: Uuid, thread_id: Uuid) {
        self.record_event(SessionEvent::TurnDiscarded {
            session_id,
            thread_id,
            timestamp: Utc::now(),
        })
        .await;
    }

    /// Records one agent node registration in the durable session graph.
    async fn record_agent_registered(&self, record: AgentRegistrationRecord) {
        self.record_event(record.into()).await;
    }

    /// Records the latest lifecycle status observed for one agent node.
    async fn record_agent_status(
        &self,
        session_id: Uuid,
        agent_id: &str,
        status: PersistedAgentStatus,
        detail: Option<&str>,
    ) {
        self.record_event(SessionEvent::AgentStatusChanged {
            session_id,
            agent_id: agent_id.to_string(),
            status,
            detail: detail.map(ToOwned::to_owned),
            timestamp: Utc::now(),
        })
        .await;
    }

    /// Records one mailbox delivery emitted by the collaboration supervisor.
    async fn record_mailbox_delivered(&self, record: MailboxDeliveryRecord) {
        self.record_event(record.into()).await;
    }
}

/// JSONL-file-based implementation of [`SessionStore`].
///
/// Writes one JSON line per turn event to a session-scoped file under
/// `~/.local/share/clawcode/sessions/YYYY/MM/DD/`.
pub struct JsonlSessionStore {
    writer: JsonlWriter,
}

impl JsonlSessionStore {
    /// Creates a new store with a system-generated session path.
    ///
    /// The generated path follows the pattern:
    /// `~/.local/share/clawcode/sessions/YYYY/MM/DD/session-{timestamp}-{uuid}.jsonl`
    pub fn create() -> Result<Self> {
        let now = Utc::now();
        let date_dir = reader::sessions_root()
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", now.month()))
            .join(format!("{:02}", now.day()));
        let timestamp = now.format("%Y%m%dT%H%M%S").to_string();
        let uuid = Uuid::new_v4();
        let filename = format!("session-{timestamp}-{uuid}.jsonl");
        let path = date_dir.join(filename);

        let writer = JsonlWriter::open(path)?;
        Ok(Self { writer })
    }

    /// Creates a new store at an explicit path (for testing).
    pub fn create_at(path: impl Into<PathBuf>) -> Result<Self> {
        let writer = JsonlWriter::open(path)?;
        Ok(Self { writer })
    }

    /// Returns the file path for display or debugging.
    pub fn path(&self) -> &Path {
        self.writer.path()
    }
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn record_event(&self, event: SessionEvent) {
        if let Err(error) = self.writer.write_event(&event).await {
            tracing::warn!("failed to persist session event: {error}");
        }
    }
}
