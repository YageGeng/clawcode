use std::sync::Arc;

use protocol::{
    AgentMessage, ContentBlock, EmbeddedResourceContent, EntryId, LaneId,
    MessageContent, MessageId, MessageIdentity, MessageTiming, SessionId,
    TimestampMs, ToolCallId, TurnId,
};
use store::{
    Clock, EntryKind, JsonlStoreFactory, NewEntry, SessionCreateOptions,
    StoreFactory,
};

/// Deterministic clock used to isolate persistence from wall-clock behavior.
struct FixedClock;

impl Clock for FixedClock {
    /// Returns an exact millisecond timestamp for the test session mutation.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(9_007_199_254_741_001_u64)
    }
}

/// JSONL persistence round-trips every MCP content form without flattening it to text.
#[test]
fn mcp_content_round_trips_through_session_store() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut store = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-mcp-content")
                .expect("session id"),
            cwd: root.path().into(),
            parent_session_id: None,
        })
        .expect("create session");
    let lane = LaneId::try_from("main").expect("lane id");
    let timestamp = TimestampMs::from(9_007_199_254_741_002_u64);
    let message = AgentMessage {
        identity: MessageIdentity {
            message_id: MessageId::try_from("message-mcp-content")
                .expect("message id"),
            turn_id: TurnId::try_from("turn-mcp-content").expect("turn id"),
        },
        timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
            .expect("message timing"),
        content: MessageContent::ToolResult {
            tool_call_id: ToolCallId::try_from("call-mcp-content")
                .expect("tool call id"),
            blocks: vec![
                ContentBlock::Audio {
                    data: "YXVkaW8=".to_string(),
                    mime_type: "audio/wav".to_string(),
                },
                ContentBlock::EmbeddedResource {
                    uri: "memory://document".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    content: EmbeddedResourceContent::Text {
                        text: "persist me".to_string(),
                    },
                },
                ContentBlock::ResourceLink {
                    uri: "file:///workspace/report.pdf".to_string(),
                    name: "report".to_string(),
                    title: Some("Report".to_string()),
                    description: Some("Generated report".to_string()),
                    mime_type: Some("application/pdf".to_string()),
                    size: Some(4_096),
                },
                ContentBlock::Structured {
                    value: serde_json::json!({ "count": 2 }),
                },
            ],
            is_error: false,
            details: None,
        },
    };
    let persisted_message = message.clone();
    let entry = store
        .append_entry(
            &lane,
            NewEntry {
                id: EntryId::try_from("entry-mcp-content").expect("entry id"),
                kind: EntryKind::Message,
                payload: serde_json::json!({ "message": persisted_message }),
            },
        )
        .expect("persist MCP content");
    let path = store.path().to_path_buf();
    drop(store);

    let reopened = factory.open(&path).expect("reopen session");
    let restored_entry =
        reopened.get_entry(&entry.id).expect("persisted entry");
    let restored: AgentMessage =
        serde_json::from_value(restored_entry.payload["message"].clone())
            .expect("deserialize persisted message");

    assert_eq!(restored, message);
}
