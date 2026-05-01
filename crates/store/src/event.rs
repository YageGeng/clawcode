use chrono::{DateTime, Utc};
use llm::{completion::Message, usage::Usage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Persists the latest lifecycle state observed for one mailbox-backed agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistedAgentStatus {
    Idle,
    Running,
    Completed,
    Failed,
    Closed,
}

impl PersistedAgentStatus {
    /// Returns the stable lowercase label used by tests and human-facing diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }
}

/// Persists the mailbox event kind that woke an ancestor agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistedMailboxEventKind {
    Spawned,
    Running,
    Completed,
    Failed,
    Closed,
}

impl PersistedMailboxEventKind {
    /// Returns the stable lowercase label used by tests and human-facing diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawned => "spawned",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }
}

/// Structured payload used to record one agent-registration event without a
/// long positional argument list.
#[derive(Debug, Clone)]
pub struct AgentRegistrationRecord {
    pub session_id: Uuid,
    pub agent_id: String,
    pub parent_agent_id: Option<String>,
    pub thread_id: Uuid,
    pub path: String,
    pub name: Option<String>,
    pub hidden_root: bool,
    pub turn_context: Option<serde_json::Value>,
}

/// Structured payload used to record one mailbox-delivery event without a long
/// positional argument list.
#[derive(Debug, Clone)]
pub struct MailboxDeliveryRecord {
    pub session_id: Uuid,
    pub recipient_agent_id: String,
    pub source_agent_id: String,
    pub source_path: String,
    pub event_id: u64,
    pub event_kind: PersistedMailboxEventKind,
    pub status: PersistedAgentStatus,
    pub message: String,
}

/// One JSONL line in a session persistence file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
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
    /// Registers one agent node in the durable session graph.
    AgentRegistered {
        session_id: Uuid,
        agent_id: String,
        parent_agent_id: Option<String>,
        thread_id: Uuid,
        path: String,
        name: Option<String>,
        hidden_root: bool,
        #[serde(default)]
        turn_context: Option<serde_json::Value>,
        timestamp: DateTime<Utc>,
    },
    /// Records the latest status observed for one agent node.
    AgentStatusChanged {
        session_id: Uuid,
        agent_id: String,
        status: PersistedAgentStatus,
        detail: Option<String>,
        timestamp: DateTime<Utc>,
    },
    /// Records one mailbox notification delivered to an ancestor agent.
    MailboxDelivered {
        session_id: Uuid,
        recipient_agent_id: String,
        source_agent_id: String,
        source_path: String,
        event_id: u64,
        event_kind: PersistedMailboxEventKind,
        status: PersistedAgentStatus,
        message: String,
        timestamp: DateTime<Utc>,
    },
}

/// Backwards-compatible alias retained for older call sites and tests.
pub type TurnEvent = SessionEvent;

impl From<AgentRegistrationRecord> for SessionEvent {
    /// Converts one structured registration payload into the persisted JSONL event.
    fn from(value: AgentRegistrationRecord) -> Self {
        Self::AgentRegistered {
            session_id: value.session_id,
            agent_id: value.agent_id,
            parent_agent_id: value.parent_agent_id,
            thread_id: value.thread_id,
            path: value.path,
            name: value.name,
            hidden_root: value.hidden_root,
            turn_context: value.turn_context,
            timestamp: Utc::now(),
        }
    }
}

impl From<MailboxDeliveryRecord> for SessionEvent {
    /// Converts one structured mailbox payload into the persisted JSONL event.
    fn from(value: MailboxDeliveryRecord) -> Self {
        Self::MailboxDelivered {
            session_id: value.session_id,
            recipient_agent_id: value.recipient_agent_id,
            source_agent_id: value.source_agent_id,
            source_path: value.source_path,
            event_id: value.event_id,
            event_kind: value.event_kind,
            status: value.status,
            message: value.message,
            timestamp: Utc::now(),
        }
    }
}
