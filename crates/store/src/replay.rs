use std::io;
use std::path::Path;

use protocol::message::Message;

use super::record::{
    AgentEdgeRecord, PersistedPayload, PersistedRecord, SessionMetaRecord,
};

/// Replayed session state loaded from a persisted JSONL file.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub struct ReplayedSession {
    /// Session metadata from the first session_meta record.
    pub meta: SessionMetaRecord,
    /// Canonical message history reconstructed from message records.
    pub messages: Vec<Message>,
    /// Live model history materialized from the latest compaction checkpoint.
    pub live_messages: Vec<Message>,
    /// Agent edges collected during replay for subagent tree restoration.
    #[builder(default)]
    pub agent_edges: Vec<AgentEdgeRecord>,
}

/// Replay a session JSONL file into metadata and canonical messages.
pub fn replay_session_file(path: &Path) -> io::Result<ReplayedSession> {
    let text = std::fs::read_to_string(path)?;
    let mut meta = None;
    let mut messages = Vec::new();
    let mut live_messages = Vec::new();
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
            PersistedPayload::SessionMeta(record) if meta.is_none() => {
                meta = Some(record)
            }
            PersistedPayload::Message(record) => {
                messages.push(record.message.clone());
                live_messages.push(record.message);
            }
            PersistedPayload::AgentEdge(record) => agent_edges.push(record),
            PersistedPayload::Compaction(record) => {
                // A checkpoint only changes future model input; the full transcript remains intact.
                live_messages = record.replacement_history;
            }
            PersistedPayload::SessionMeta(_)
            | PersistedPayload::TurnContext(_)
            | PersistedPayload::TurnComplete(_)
            | PersistedPayload::TurnAborted(_) => {}
        }
    }
    let meta = meta.ok_or_else(|| {
        io::Error::other("session file missing session_meta record")
    })?;
    Ok(ReplayedSession::builder()
        .meta(meta)
        .messages(messages)
        .live_messages(live_messages)
        .agent_edges(agent_edges)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use protocol::message::Message;

    use crate::record::{
        CompactionRecord, MessageRecord, PersistedPayload, PersistedRecord,
        SCHEMA_VERSION, timestamp_now,
    };

    /// Build a persisted JSONL record for replay tests.
    fn record(payload: PersistedPayload) -> PersistedRecord {
        PersistedRecord::builder()
            .timestamp(timestamp_now())
            .schema_version(SCHEMA_VERSION)
            .payload(payload)
            .build()
    }

    /// Replay keeps the full transcript but materializes live history from the latest checkpoint.
    #[test]
    fn replay_preserves_full_messages_and_uses_compacted_live_messages() {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        let meta = SessionMetaRecord::builder()
            .session_id(protocol::SessionId::from("session"))
            .agent_path(protocol::AgentPath::root())
            .cwd(std::path::PathBuf::from("/tmp/project"))
            .provider_id("provider".to_string())
            .model_id("model".to_string())
            .base_system_prompt("base".to_string())
            .created_at(timestamp_now())
            .build();
        let old = Message::user("old");
        let summary = Message::user("summary");
        let tail = Message::assistant("tail");
        let after = Message::user("after");

        for payload in [
            PersistedPayload::SessionMeta(meta),
            PersistedPayload::Message(
                MessageRecord::builder()
                    .turn_id("turn-1".to_string())
                    .message(old.clone())
                    .build(),
            ),
            PersistedPayload::Compaction(
                CompactionRecord::builder()
                    .turn_id("compact-1".to_string())
                    .summary("summary".to_string())
                    .replacement_history(vec![summary.clone(), tail.clone()])
                    .retained_message_count(1)
                    .build(),
            ),
            PersistedPayload::Message(
                MessageRecord::builder()
                    .turn_id("turn-2".to_string())
                    .message(after.clone())
                    .build(),
            ),
        ] {
            writeln!(
                file,
                "{}",
                serde_json::to_string(&record(payload)).expect("serialize")
            )
            .expect("write");
        }

        let replayed = replay_session_file(file.path()).expect("replay");

        assert_eq!(replayed.messages, vec![old, after.clone()]);
        assert_eq!(replayed.live_messages, vec![summary, tail, after]);
    }
}
