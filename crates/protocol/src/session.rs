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

/// Prefix-free user-bash input shared by Kernel and ACP request handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserBashInput {
    /// Shell command supplied after `!` or `!!`.
    pub command: String,
    /// Whether the resulting transcript message is excluded from model context.
    #[serde(default)]
    pub exclude_from_context: bool,
}

impl UserBashInput {
    /// Parses a non-empty Pi `!` or `!!` command while preserving ordinary prompt text.
    #[must_use]
    pub fn parse_prefixed(input: &str) -> Option<Self> {
        let exclude_from_context = input.starts_with("!!");
        let command = input
            .strip_prefix(if exclude_from_context { "!!" } else { "!" })?
            .trim();
        (!command.is_empty()).then(|| Self {
            command: command.to_string(),
            exclude_from_context,
        })
    }
}

/// Session-bound request for one server-side user-bash execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserBashRequest {
    /// Session whose server-side working directory owns the command.
    pub session_id: SessionId,
    /// Shell command supplied without a `!` prefix.
    pub command: String,
    /// Whether the resulting transcript message is excluded from model context.
    #[serde(default)]
    pub exclude_from_context: bool,
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
