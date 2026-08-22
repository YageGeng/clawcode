use std::fs;
use std::io::Write;
use std::sync::Arc;

use protocol::{EntryId, LaneId, RecordId, RunId, SessionId, TimestampMs};
use store::{
    Clock, EntryKind, JsonlStoreFactory, NewEntry, NewRecord, RecordKind,
    SessionCreateOptions, SessionForkOptions, StoreError, StoreFactory,
};

/// Deterministic clock used to assert exact persisted timestamp strings.
struct FixedClock;

impl Clock for FixedClock {
    /// Returns a timestamp beyond JavaScript's safe integer limit.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(9_007_199_254_740_993_u64)
    }
}

/// Creating a session writes only a v4 header inside its cwd-encoded directory.
#[test]
fn create_session_writes_v4_header_without_global_index() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let store = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-1")
                .expect("valid session id"),
            cwd: "/home/user/project".into(),
            parent_session_id: None,
        })
        .expect("session should be created");

    let content = fs::read_to_string(store.path()).expect("read session file");
    let header: serde_json::Value = serde_json::from_str(content.trim_end())
        .expect("header should be JSON");

    assert_eq!(header["kind"], "header");
    assert_eq!(header["version"], 4);
    assert_eq!(header["id"], "session-1");
    assert_eq!(header["createdAt"], "9007199254740993");
    assert_eq!(header["cwd"], "/home/user/project");
    assert!(
        store
            .path()
            .parent()
            .expect("session directory")
            .ends_with("--home-user-project--")
    );
    assert!(!root.path().join("index.jsonl").exists());
}

/// Appending entries assigns sequence, timestamp, and the lane's current parent.
#[test]
fn append_entry_moves_lane_and_links_parent() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut store = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-1")
                .expect("valid session id"),
            cwd: root.path().into(),
            parent_session_id: None,
        })
        .expect("session should be created");

    let lane = LaneId::try_from("main").expect("valid lane id");
    let first = store
        .append_entry(
            &lane,
            NewEntry {
                id: EntryId::try_from("entry-1").expect("valid entry id"),
                kind: EntryKind::Custom,
                payload: serde_json::json!({
                    "customType": "test",
                    "data": { "value": 1 }
                }),
            },
        )
        .expect("append first entry");
    let second = store
        .append_entry(
            &lane,
            NewEntry {
                id: EntryId::try_from("entry-2").expect("valid entry id"),
                kind: EntryKind::Message,
                payload: serde_json::json!({ "message": { "role": "user" } }),
            },
        )
        .expect("append second entry");

    assert_eq!(first.sequence.get(), 1);
    assert_eq!(first.parent_id, None);
    assert_eq!(second.sequence.get(), 2);
    assert_eq!(
        second.parent_id.as_ref().map(EntryId::as_str),
        Some("entry-1")
    );
    assert_eq!(
        store.lane(&lane).map(|entry| entry.as_str()),
        Some("entry-2")
    );
}

/// A rejected append must not consume the shared session sequence.
///
/// The filesystem store holds its append handle open for the session lifetime,
/// so write failures surface at construction; a mutation rejected by candidate
/// validation (duplicate id) must still leave the sequence available for retry.
#[test]
fn rejected_append_keeps_sequence_available_for_retry_and_reopen() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut store = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-retry")
                .expect("valid session id"),
            cwd: root.path().into(),
            parent_session_id: None,
        })
        .expect("session should be created");
    let lane = LaneId::try_from("main").expect("valid lane id");
    let path = store.path().to_path_buf();

    // A duplicate entry id is rejected by candidate validation before any
    // durable write, so the allocated sequence must remain available.
    let duplicate = store.append_entry(
        &lane,
        NewEntry {
            id: EntryId::try_from("entry-failed").expect("valid entry id"),
            kind: EntryKind::Custom,
            payload: serde_json::json!({ "customType": "failed" }),
        },
    );
    duplicate.expect("first append should succeed");
    let rejected = store.append_entry(
        &lane,
        NewEntry {
            id: EntryId::try_from("entry-failed").expect("valid entry id"),
            kind: EntryKind::Custom,
            payload: serde_json::json!({ "customType": "rejected" }),
        },
    );
    assert!(matches!(rejected, Err(StoreError::DuplicateEntry(_))));

    let stored = store
        .append_entry(
            &lane,
            NewEntry {
                id: EntryId::try_from("entry-retry").expect("valid entry id"),
                kind: EntryKind::Custom,
                payload: serde_json::json!({ "customType": "retry" }),
            },
        )
        .expect("retry append should succeed");
    assert_eq!(stored.sequence.get(), 2);
    drop(store);

    let reopened = factory.open(&path).expect("retry log should reopen");
    assert_eq!(reopened.entries().len(), 2);
}

/// Lane records and global facts share the same sequence and JSONL file.
#[test]
fn append_record_and_facts_use_pi_v4_mutations() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut store = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-1")
                .expect("valid session id"),
            cwd: root.path().into(),
            parent_session_id: None,
        })
        .expect("session should be created");
    let lane = LaneId::try_from("main").expect("valid lane id");
    store
        .append_entry(
            &lane,
            NewEntry {
                id: EntryId::try_from("entry-1").expect("valid entry id"),
                kind: EntryKind::Custom,
                payload: serde_json::json!({ "customType": "test" }),
            },
        )
        .expect("append label target");

    let record = store
        .append_record(NewRecord {
            id: RecordId::try_from("record-1").expect("valid record id"),
            lane,
            run_id: Some(RunId::try_from("run-1").expect("valid run id")),
            kind: RecordKind::OperationFinished,
            payload: serde_json::json!({ "outcome": "completed" }),
        })
        .expect("append operation record");
    store
        .set_name(Some("Investigation".to_string()))
        .expect("set session name");
    store
        .set_label(
            EntryId::try_from("entry-1").expect("valid entry id"),
            Some("checkpoint".to_string()),
        )
        .expect("set entry label");

    assert_eq!(record.sequence.get(), 2);
    assert_eq!(store.last_sequence(), 4);
    let content = fs::read_to_string(store.path()).expect("read session file");
    let lines = content.lines().collect::<Vec<_>>();
    let record_line: serde_json::Value =
        serde_json::from_str(lines[2]).expect("record line");
    let name_line: serde_json::Value =
        serde_json::from_str(lines[3]).expect("name fact line");
    let label_line: serde_json::Value =
        serde_json::from_str(lines[4]).expect("label fact line");

    assert_eq!(record_line["kind"], "record");
    assert_eq!(record_line["type"], "operation_finished");
    assert_eq!(record_line["timestamp"], "9007199254740993");
    assert_eq!(name_line["fact"], "name");
    assert_eq!(label_line["fact"], "label");

    let path = store.path().to_path_buf();
    drop(store);
    let reopened = factory.open(&path).expect("reopen session");
    assert_eq!(reopened.last_sequence(), 4);
}

/// Reopening a session repairs an unacknowledged partial final append.
#[test]
fn open_repairs_torn_json_tail_and_recovers_lanes() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut store = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-1")
                .expect("valid session id"),
            cwd: root.path().into(),
            parent_session_id: None,
        })
        .expect("session should be created");
    let lane = LaneId::try_from("main").expect("valid lane id");
    store
        .append_entry(
            &lane,
            NewEntry {
                id: EntryId::try_from("entry-1").expect("valid entry id"),
                kind: EntryKind::Custom,
                payload: serde_json::json!({ "customType": "test" }),
            },
        )
        .expect("append entry");
    let path = store.path().to_path_buf();
    drop(store);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open tail")
        .write_all(b"{\"kind\":")
        .expect("append torn tail");

    let reopened = factory.open(&path).expect("repair and reopen session");

    assert_eq!(
        reopened.lane(&lane).map(|entry| entry.as_str()),
        Some("entry-1")
    );
    assert!(
        fs::read_to_string(path)
            .expect("read repaired file")
            .ends_with('\n')
    );
}

/// Fork copies only the selected ancestor path and records the source session id.
#[test]
fn fork_copies_selected_branch_and_sets_parent_session() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut source = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("source-session")
                .expect("valid session id"),
            cwd: root.path().into(),
            parent_session_id: None,
        })
        .expect("source should be created");
    let main = LaneId::try_from("main").expect("valid lane id");
    source
        .append_entry(
            &main,
            NewEntry {
                id: EntryId::try_from("entry-1").expect("valid entry id"),
                kind: EntryKind::Message,
                payload: serde_json::json!({ "message": { "role": "user" } }),
            },
        )
        .expect("append root entry");
    source
        .append_entry(
            &main,
            NewEntry {
                id: EntryId::try_from("entry-2").expect("valid entry id"),
                kind: EntryKind::Message,
                payload: serde_json::json!({ "message": { "role": "assistant" } }),
            },
        )
        .expect("append selected leaf");
    let alternate = LaneId::try_from("alternate").expect("valid lane id");
    source
        .create_lane(
            alternate.clone(),
            Some(EntryId::try_from("entry-1").expect("valid entry id")),
        )
        .expect("create alternate lane");
    source
        .append_entry(
            &alternate,
            NewEntry {
                id: EntryId::try_from("entry-3").expect("valid entry id"),
                kind: EntryKind::Custom,
                payload: serde_json::json!({ "customType": "unrelated" }),
            },
        )
        .expect("append unrelated branch");
    let source_path = source.path().to_path_buf();
    drop(source);

    let fork = factory
        .fork(
            &source_path,
            SessionForkOptions {
                create: SessionCreateOptions {
                    session_id: SessionId::try_from("fork-session")
                        .expect("valid session id"),
                    cwd: root.path().into(),
                    parent_session_id: None,
                },
                source_leaf: EntryId::try_from("entry-2")
                    .expect("valid entry id"),
                lane: main.clone(),
            },
        )
        .expect("fork selected branch");

    assert_eq!(fork.lane(&main).map(EntryId::as_str), Some("entry-2"));
    assert!(
        fork.get_entry(&EntryId::try_from("entry-1").expect("valid entry id"))
            .is_some()
    );
    assert!(
        fork.get_entry(&EntryId::try_from("entry-3").expect("valid entry id"))
            .is_none()
    );
    let content = fs::read_to_string(fork.path()).expect("read fork file");
    let header: serde_json::Value =
        serde_json::from_str(content.lines().next().expect("fork header"))
            .expect("header should be JSON");
    assert_eq!(header["parentSessionId"], "source-session");
}

/// Listing scans v4 headers directly and reopening restores facts and branch movement.
#[test]
fn list_and_reopen_restore_complete_session_state_without_index() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let cwd = root.path().join("workspace");
    let mut store = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-state")
                .expect("valid session id"),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .expect("session should be created");
    let main = LaneId::try_from("main").expect("valid lane id");
    let first_id = EntryId::try_from("entry-root").expect("valid entry id");
    store
        .append_entry(
            &main,
            NewEntry {
                id: first_id.clone(),
                kind: EntryKind::Custom,
                payload: serde_json::json!({ "customType": "root" }),
            },
        )
        .expect("append root");
    store
        .set_name(Some("Persisted name".to_string()))
        .expect("set name");
    store
        .set_label(first_id.clone(), Some("checkpoint".to_string()))
        .expect("set label");
    store.move_lane(&main, None).expect("move lane to root");
    let path = store.path().to_path_buf();
    drop(store);

    let listed = factory.list(Some(&cwd)).expect("list sessions");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id.as_str(), "session-state");
    assert_eq!(listed[0].cwd, cwd);

    let reopened = factory.open(&path).expect("reopen session");
    assert_eq!(reopened.lane(&main), None);
    assert_eq!(reopened.name(), Some("Persisted name"));
    assert_eq!(reopened.label(&first_id), Some("checkpoint"));
    assert_eq!(reopened.entries().len(), 1);
}

/// Branch reads walk parent links independently from the active lane pointer.
#[test]
fn branch_read_returns_root_to_leaf_order() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut store = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-branch")
                .expect("valid session id"),
            cwd: root.path().into(),
            parent_session_id: None,
        })
        .expect("session should be created");
    let main = LaneId::try_from("main").expect("valid lane id");
    for id in ["entry-1", "entry-2", "entry-3"] {
        store
            .append_entry(
                &main,
                NewEntry {
                    id: EntryId::try_from(id).expect("valid entry id"),
                    kind: EntryKind::Custom,
                    payload: serde_json::json!({ "customType": id }),
                },
            )
            .expect("append entry");
    }

    let branch = store
        .branch(&EntryId::try_from("entry-2").expect("valid leaf"))
        .expect("read branch");
    assert_eq!(
        branch
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["entry-1", "entry-2"]
    );
}

/// Listing resolves the latest name fact without creating or updating an index.
#[test]
fn list_returns_latest_name_fact_without_an_index_file() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut session = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-named")
                .expect("session id"),
            cwd: root.path().to_path_buf(),
            parent_session_id: None,
        })
        .expect("create");
    session
        .set_name(Some("First".to_string()))
        .expect("first name");
    session
        .set_name(Some("Final title".to_string()))
        .expect("final name");
    drop(session);

    let sessions = factory.list(None).expect("list");
    assert_eq!(sessions[0].name.as_deref(), Some("Final title"));
    assert!(!root.path().join("index.jsonl").exists());
}

/// Invalid compaction data is rejected before it can corrupt the durable log.
#[test]
fn append_rejects_compaction_without_required_details_before_persistence() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut session = factory
        .create(SessionCreateOptions {
            session_id: SessionId::try_from("session-bad-compaction")
                .expect("session id"),
            cwd: root.path().to_path_buf(),
            parent_session_id: None,
        })
        .expect("create");
    let error = session
        .append_entry(
            &LaneId::try_from("main").expect("lane"),
            NewEntry {
                id: EntryId::try_from("compaction-1").expect("entry"),
                kind: EntryKind::Compaction,
                payload: serde_json::json!({
                    "summary": "summary",
                    "retainedTail": [],
                    "tokensBefore": 12
                }),
            },
        )
        .expect_err("invalid compaction must be rejected before append");
    assert!(matches!(error, store::StoreError::InvalidSession(_)));
    let path = session.path().to_path_buf();
    drop(session);

    let reopened = factory.open(&path).expect("log should remain valid");
    assert!(reopened.entries().is_empty());
}

/// Deleting a session removes its JSONL log and remains successful when repeated.
#[test]
fn delete_session_removes_history_idempotently() {
    let root = tempfile::tempdir().expect("temporary root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let session_id =
        SessionId::try_from("session-delete").expect("valid session id");
    let store = factory
        .create(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: root.path().join("workspace"),
            parent_session_id: None,
        })
        .expect("create session");
    let path = store.path().to_path_buf();
    drop(store);

    factory.delete(&session_id).expect("delete session");

    assert!(!path.exists());
    assert!(factory.list(None).expect("list after delete").is_empty());
    factory
        .delete(&session_id)
        .expect("repeated delete remains successful");
}
