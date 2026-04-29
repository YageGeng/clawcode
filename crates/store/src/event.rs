use chrono::{DateTime, Utc};
use llm::{completion::Message, usage::Usage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One JSONL line in a session persistence file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    /// Marks the start of a new turn within a session.
    TurnStarted {
        session_id: Uuid,
        thread_id: Uuid,
        user_text: String,
        message: Message,
        timestamp: DateTime<Utc>,
    },
    /// A message appended to the active turn transcript.
    Message {
        session_id: Uuid,
        thread_id: Uuid,
        message: Message,
        timestamp: DateTime<Utc>,
    },
    /// The turn has been finalized with usage statistics and a context snapshot.
    TurnCompleted {
        session_id: Uuid,
        thread_id: Uuid,
        usage: Usage,
        context_item: serde_json::Value,
        timestamp: DateTime<Utc>,
    },
    /// The active turn was discarded (e.g. due to an error).
    TurnDiscarded {
        session_id: Uuid,
        thread_id: Uuid,
        timestamp: DateTime<Utc>,
    },
}
