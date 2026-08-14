use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    AgentMessage, EntryId, LaneId, RunId, SessionId, TimestampMs, TurnRecord,
};

/// Input required to start one serialized agent run in an existing session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    /// Existing session that owns the run.
    pub session_id: SessionId,
    /// Initial user text assigned to the run's first Turn.
    pub input: String,
}

/// Complete in-process result returned after one run settles.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// Stable run identifier shared by all produced Turns.
    pub run_id: RunId,
    /// Messages produced or consumed by this run in persisted order.
    pub messages: Vec<AgentMessage>,
    /// Completed Turns in execution order.
    pub turns: Vec<TurnRecord>,
}

/// Persisted session metadata exposed without leaking a storage implementation.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct SessionSummary {
    /// Stable session identifier.
    pub session_id: SessionId,
    /// Working directory recorded in the v4 header.
    pub cwd: PathBuf,
    /// Session creation timestamp in Unix milliseconds.
    pub created_at_ms: TimestampMs,
    /// Last JSONL modification timestamp in Unix milliseconds.
    pub modified_at_ms: TimestampMs,
    /// Source session when this session was forked.
    #[builder(default)]
    pub parent_session_id: Option<SessionId>,
    /// Latest persisted session name fact.
    #[builder(default)]
    pub name: Option<String>,
}

/// One persisted entry exposed for tree rendering and navigation.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeEntry {
    /// Stable entry identifier.
    pub entry_id: EntryId,
    /// Parent entry on the branch, or root when absent.
    #[builder(default)]
    pub parent_id: Option<EntryId>,
    /// Stable pi v4 entry discriminator.
    pub kind: String,
    /// Storage-assigned Unix millisecond timestamp.
    pub timestamp_ms: TimestampMs,
    /// Kind-specific JSON payload.
    pub payload: serde_json::Value,
}

/// Session-wide tree snapshot plus the active lane cursor.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, typed_builder::TypedBuilder,
)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeSnapshot {
    /// Session being inspected.
    pub session_id: SessionId,
    /// Active lane name.
    pub lane: LaneId,
    /// Current lane leaf.
    #[builder(default)]
    pub leaf_id: Option<EntryId>,
    /// All entries in shared sequence order.
    pub entries: Vec<SessionTreeEntry>,
    /// Latest global session name fact.
    #[builder(default)]
    pub name: Option<String>,
}
