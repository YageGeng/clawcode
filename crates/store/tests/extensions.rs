use std::sync::Arc;

use protocol::{
    EntryId, ExtensionEntryData, ExtensionId, LaneId, SessionId, TimestampMs,
};
use store::{
    Clock, EntryKind, JsonlStoreFactory, SessionCreateOptions,
    SessionForkOptions, StoreFactory,
};

/// Deterministic clock used by extension-entry persistence assertions.
struct FixedClock;

impl Clock for FixedClock {
    /// Returns one precision-safe timestamp beyond JavaScript's integer range.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(9_007_199_254_740_993_u64)
    }
}

/// Extension entries survive reopen and selected-branch fork without an index file.
#[test]
fn extension_entries_persist_replay_and_fork_as_pi_v4_custom_entries() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let lane = LaneId::try_from("main").expect("lane id");
    let mut session = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-extension")
                .expect("session id"),
            cwd: root.path().join("workspace"),
            parent_session_id: None,
        })
        .expect("create session");
    let entry = session
        .append_extension_entry(
            &lane,
            EntryId::try_from("entry-extension").expect("entry id"),
            ExtensionEntryData {
                extension_id: ExtensionId::try_from("audit")
                    .expect("extension id"),
                custom_type: "checkpoint".to_string(),
                data: serde_json::json!({ "approved": true }),
            },
        )
        .expect("append extension entry");
    let session_path = session.path().to_path_buf();
    drop(session);

    assert_eq!(entry.kind, EntryKind::Custom);
    assert_eq!(entry.payload["extensionId"], "audit");
    assert_eq!(entry.payload["customType"], "checkpoint");
    assert_eq!(entry.timestamp_ms.to_string(), "9007199254740993");

    let reopened = factory.open(&session_path).expect("reopen session");
    let replayed = reopened.entries();
    assert_eq!(replayed, vec![entry.clone()]);
    drop(reopened);

    let fork = factory
        .fork(
            &session_path,
            SessionForkOptions {
                create: SessionCreateOptions {
                    session_id: SessionId::try_from("session-fork")
                        .expect("fork session id"),
                    cwd: root.path().join("workspace"),
                    parent_session_id: None,
                },
                source_leaf: entry.id.clone(),
                lane,
            },
        )
        .expect("fork extension branch");
    assert_eq!(fork.entries(), vec![entry]);
    assert!(!root.path().join("index.jsonl").exists());
}
