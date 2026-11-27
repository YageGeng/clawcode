//! Forking carries the request contents a copied branch's snapshots address.

use std::sync::Arc;

use protocol::{EntryId, LaneId, RecordId, RunId, SessionId, TimestampMs};
use store::{
    Clock, EntryKind, JsonlStoreFactory, NewEntry, NewRecord, RecordKind,
    SessionCreateOptions, SessionForkOptions, StoreFactory,
};

/// Deterministic clock for the stores in this file.
struct FixedClock;

impl Clock for FixedClock {
    /// Returns a fixed timestamp.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(1_000)
    }
}

/// A message entry with `payload`.
fn message(id: &str, payload: serde_json::Value) -> NewEntry {
    NewEntry {
        id: EntryId::try_from(id).expect("entry id"),
        kind: EntryKind::Message,
        payload,
    }
}

/// A fork carries only the request contents its copied requests address,
/// once each, and logs them before the entries, as the source did; contents
/// of the branch it leaves behind stay there.
#[test]
fn fork_copies_the_contents_its_branch_addresses() {
    let root = tempfile::tempdir().expect("root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(FixedClock));
    let mut source = factory
        .create(
            SessionCreateOptions::builder()
                .session_id(SessionId::try_from("source").expect("id"))
                .cwd(root.path().into())
                .build(),
        )
        .expect("source");
    let main = LaneId::try_from("main").expect("lane");
    let content = |id: &str, hash: &str| {
        NewRecord::builder()
            .id(RecordId::try_from(id).expect("record id"))
            .lane(main.clone())
            .run_id(Some(RunId::try_from("run-1").expect("run id")))
            .kind(RecordKind::RequestContent)
            .payload(serde_json::json!({
                "requestContent": {
                    "hash": hash,
                    "kind": "system_prompt",
                    "content": id,
                },
            }))
            .build()
    };
    // An assistant entry whose request addresses `prompt` and `tools`.
    let answer = |prompt: &str, tools: &str| {
        serde_json::json!({ "content": { "metadata": { "request": {
            "systemPrompt": prompt,
            "tools": tools,
        } } } })
    };
    let mut append = |id: &str, payload| {
        source
            .append_entry(&main, message(id, payload))
            .expect("entry");
    };
    append(
        "entry-0",
        serde_json::json!({ "message": { "role": "user" } }),
    );
    source
        .append_record(content("content-a", "a"))
        .expect("content");
    source
        .append_record(content("content-t", "t"))
        .expect("content");
    source
        .append_entry(&main, message("entry-1", answer("a", "t")))
        .expect("entry");
    source
        .append_record(content("content-b", "b"))
        .expect("content");
    source
        .append_entry(&main, message("entry-2", answer("b", "t")))
        .expect("entry");
    let source_path = source.path().to_path_buf();
    drop(source);

    let mut fork = factory
        .fork(
            &source_path,
            SessionForkOptions {
                create: SessionCreateOptions::builder()
                    .session_id(SessionId::try_from("fork").expect("id"))
                    .cwd(root.path().into())
                    .build(),
                source_leaf: EntryId::try_from("entry-1").expect("entry id"),
                lane: main,
            },
        )
        .expect("fork");

    let copied: Vec<String> = fork
        .records()
        .into_iter()
        .filter(|record| record.kind == RecordKind::RequestContent)
        .inspect(|record| assert!(record.run_id.is_none()))
        .filter_map(|record| {
            record
                .payload
                .get("requestContent")?
                .get("hash")?
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(copied, ["a", "t"]);

    // The contents come before the first copied entry in the fork's log.
    let log = std::fs::read_to_string(fork.path()).expect("log");
    let line = |needle: &str| {
        log.lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} not logged"))
    };
    assert!(line("content-t") < line("entry-0"));

    // Replaying copied request contents and a linear branch cannot establish
    // local authorship. A subsequent name fact belongs only to this fork.
    assert_eq!(fork.first_local_sequence(), None);
    fork.set_name(Some("local name".into())).expect("rename");
    let boundary = Some(fork.last_sequence());
    assert_eq!(fork.first_local_sequence(), boundary);
    let reopened = factory.open(fork.path()).expect("reopen");
    assert_eq!(reopened.first_local_sequence(), boundary);
}
