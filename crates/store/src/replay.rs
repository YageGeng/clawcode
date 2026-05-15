use std::io;
use std::path::Path;

use protocol::message::Message;

use super::record::{AgentEdgeRecord, PersistedPayload, PersistedRecord, SessionMetaRecord};

/// Replayed session state loaded from a persisted JSONL file.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub struct ReplayedSession {
    /// Session metadata from the first session_meta record.
    pub meta: SessionMetaRecord,
    /// Canonical message history reconstructed from message records.
    pub messages: Vec<Message>,
    /// Agent edges collected during replay for subagent tree restoration.
    #[builder(default)]
    pub agent_edges: Vec<AgentEdgeRecord>,
}

/// Replay a session JSONL file into metadata and canonical messages.
pub fn replay_session_file(path: &Path) -> io::Result<ReplayedSession> {
    let text = std::fs::read_to_string(path)?;
    let mut meta = None;
    let mut messages = Vec::new();
    let mut agent_edges = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Corrupt lines are ignored so one partial write does not make the full session unloadable.
        let Ok(record) = serde_json::from_str::<PersistedRecord>(line) else {
            tracing::warn!(path = %path.display(), "skipping corrupt session record");
            continue;
        };
        match record.payload {
            PersistedPayload::SessionMeta(record) if meta.is_none() => meta = Some(record),
            PersistedPayload::Message(record) => messages.push(record.message),
            PersistedPayload::AgentEdge(record) => agent_edges.push(record),
            PersistedPayload::SessionMeta(_)
            | PersistedPayload::TurnContext(_)
            | PersistedPayload::TurnComplete(_)
            | PersistedPayload::TurnAborted(_) => {}
        }
    }
    let meta = meta.ok_or_else(|| io::Error::other("session file missing session_meta record"))?;
    Ok(ReplayedSession::builder()
        .meta(meta)
        .messages(messages)
        .agent_edges(agent_edges)
        .build())
}
