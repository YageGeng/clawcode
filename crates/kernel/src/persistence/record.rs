use std::path::PathBuf;

use protocol::message::Message;
use protocol::{AgentPath, SessionId, StopReason};
use serde::{Deserialize, Serialize};

/// Current JSONL schema version used for session persistence records.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// A timestamped JSONL record stored in a session rollout file.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub(crate) struct PersistedRecord {
    /// UTC-ish process timestamp when the record was written.
    pub timestamp: String,
    /// Schema version for forward-compatible replay.
    pub schema_version: u32,
    /// Typed payload for session replay.
    pub payload: PersistedPayload,
}

/// Replayable payloads written to the session JSONL file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub(crate) enum PersistedPayload {
    /// Immutable metadata written once when the session file is created.
    SessionMeta(SessionMetaRecord),
    /// Durable prompt/runtime settings for one turn.
    TurnContext(TurnContextRecord),
    /// Canonical conversation message accepted into history.
    Message(MessageRecord),
    /// Completion marker for a successfully finished turn.
    TurnComplete(TurnCompleteRecord),
    /// Completion marker for an interrupted or failed turn.
    TurnAborted(TurnAbortedRecord),
    /// Parent-child edge used for future subagent discovery.
    AgentEdge(AgentEdgeRecord),
}

/// Immutable metadata captured when a session rollout is created.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub(crate) struct SessionMetaRecord {
    /// Session id used by protocol and kernel APIs.
    pub session_id: SessionId,
    /// Optional parent session id when this session is a subagent.
    #[builder(default, setter(strip_option))]
    pub parent_session_id: Option<SessionId>,
    /// Agent path for root or subagent routing.
    pub agent_path: AgentPath,
    /// Optional agent role name for subagent sessions.
    #[builder(default, setter(strip_option))]
    pub agent_role: Option<String>,
    /// Optional human-friendly nickname for subagent display.
    #[builder(default, setter(strip_option))]
    pub agent_nickname: Option<String>,
    /// Working directory used to create the session.
    pub cwd: PathBuf,
    /// Provider id selected at session creation time.
    pub provider_id: String,
    /// Model id selected at session creation time.
    pub model_id: String,
    /// Rendered base system prompt used by the first turn.
    pub base_system_prompt: String,
    /// Timestamp when the session was created.
    pub created_at: String,
}

/// Durable snapshot of prompt/runtime settings for one turn.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub(crate) struct TurnContextRecord {
    /// Stable id for this turn inside the session.
    pub turn_id: String,
    /// User-visible operation kind, such as prompt or inter-agent message.
    pub kind: TurnKindRecord,
    /// Current working directory for this turn.
    pub cwd: PathBuf,
    /// Provider id used by this turn.
    pub provider_id: String,
    /// Model id used by this turn.
    pub model_id: String,
    /// Fully rendered preamble passed to CompletionRequest.
    pub rendered_preamble: String,
    /// Optional ad-hoc system prompt from Op::Prompt.
    #[builder(default, setter(strip_option))]
    pub user_system_prompt: Option<String>,
}

/// Durable turn source classification.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnKindRecord {
    /// A normal user prompt submitted to the session.
    Prompt,
    /// A message delivered from another agent.
    InterAgentMessage,
}

/// A replayable conversation message appended to ContextManager.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub(crate) struct MessageRecord {
    /// Turn id that produced or consumed this message.
    pub turn_id: String,
    /// Message persisted after it is accepted into ContextManager.
    pub message: Message,
}

/// Marks a turn as durably completed.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub(crate) struct TurnCompleteRecord {
    /// Completed turn id.
    pub turn_id: String,
    /// Stop reason emitted to protocol clients.
    pub stop_reason: StopReason,
}

/// Marks a turn as interrupted before normal completion.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub(crate) struct TurnAbortedRecord {
    /// Aborted turn id.
    pub turn_id: String,
    /// Human-readable abort reason.
    pub reason: String,
}

/// Durable parent-child edge for subagent discovery and resume.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub(crate) struct AgentEdgeRecord {
    /// Parent session id.
    pub parent_session_id: SessionId,
    /// Child session id.
    pub child_session_id: SessionId,
    /// Child agent path under the parent tree.
    pub child_agent_path: AgentPath,
    /// Child role name.
    pub child_role: String,
    /// Edge lifecycle status.
    pub status: AgentEdgeStatusRecord,
}

/// Durable lifecycle status for an agent edge.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentEdgeStatusRecord {
    /// The parent-child edge is active.
    Open,
    /// The child was explicitly closed.
    Closed,
}

/// Build an RFC3339 UTC timestamp string for persistence records.
pub(crate) fn timestamp_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
