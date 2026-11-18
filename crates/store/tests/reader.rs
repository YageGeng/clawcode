use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::{EntryId, LaneId, RecordId, RunId, SessionId};
use store::{
    EntryKind, JsonlStoreFactory, LogMutation, LoggedMutation, MutationReader,
    NewEntry, NewRecord, RecordKind, SessionCreateOptions, StoreError,
    StoreFactory, SystemClock,
};

/// Writes one Session that exercises every mutation kind and returns its log path.
fn write_session(
    factory: &JsonlStoreFactory,
    cwd: PathBuf,
    id: &str,
) -> PathBuf {
    let mut store = factory
        .create(
            SessionCreateOptions::builder()
                .session_id(SessionId::try_from(id).expect("id"))
                .cwd(cwd)
                .build(),
        )
        .expect("create");
    let main = LaneId::try_from("main").expect("lane");
    let first = EntryId::try_from("entry-1").expect("entry");
    for entry_id in ["entry-1", "entry-2"] {
        store
            .append_entry(
                &main,
                NewEntry {
                    id: EntryId::try_from(entry_id).expect("entry"),
                    kind: EntryKind::Custom,
                    payload: serde_json::json!({"customType": "note"}),
                },
            )
            .expect("entry");
    }
    store
        .append_record(
            NewRecord::builder()
                .id(RecordId::try_from("record-1").expect("record"))
                .lane(main.clone())
                .run_id(Some(RunId::try_from("run-1").expect("run")))
                .kind(RecordKind::OperationStarted)
                .payload(serde_json::json!({"operation": "agent"}))
                .build(),
        )
        .expect("record");
    store.set_name(Some("Reader".into())).expect("name");
    store
        .create_lane(
            LaneId::try_from("side").expect("lane"),
            Some(first.clone()),
        )
        .expect("lane");
    store.set_label(first, Some("start".into())).expect("label");
    store.sync().expect("sync");
    store.path().to_path_buf()
}

/// Collects every item a reader yields, failing the test on decode errors.
fn read_all(bytes: &[u8], base_offset: u64) -> Vec<LoggedMutation> {
    MutationReader::new(bytes, base_offset)
        .collect::<Result<Vec<_>, _>>()
        .expect("decode")
}

/// The header comes first and mutation offsets tile the file exactly.
#[test]
fn reader_yields_header_then_every_mutation_in_order() {
    let root = tempfile::tempdir().expect("root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(SystemClock));
    let path =
        write_session(&factory, root.path().join("project"), "session-r");
    let bytes = fs::read(&path).expect("read");
    let items = read_all(&bytes, 0);

    let Some(LogMutation::Header(header)) =
        items.first().map(|item| &item.mutation)
    else {
        panic!("first item must be the header");
    };
    assert_eq!(header.id.as_str(), "session-r");
    let sequences = items
        .iter()
        .skip(1)
        .map(|item| item.mutation.sequence().expect("sequence"))
        .collect::<Vec<_>>();
    assert_eq!(
        sequences,
        (1..=u64::try_from(items.len() - 1).expect("len")).collect::<Vec<_>>()
    );
    for pair in items.windows(2) {
        assert_eq!(pair[0].next_offset, pair[1].offset);
    }
    assert_eq!(items.last().expect("last").next_offset, bytes.len() as u64);
}

/// A reader started at a line offset yields the same suffix with absolute offsets.
#[test]
fn reader_resumes_from_any_line_offset() {
    let root = tempfile::tempdir().expect("root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(SystemClock));
    let path =
        write_session(&factory, root.path().join("project"), "session-r");
    let bytes = fs::read(&path).expect("read");
    let items = read_all(&bytes, 0);
    let offset = items[2].offset;
    let start = usize::try_from(offset).expect("offset");

    let resumed = read_all(&bytes[start..], offset);

    assert_eq!(resumed, items[2..].to_vec());
}

/// An unterminated final line is left for a later read.
#[test]
fn reader_stops_before_torn_tail() {
    let root = tempfile::tempdir().expect("root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(SystemClock));
    let path =
        write_session(&factory, root.path().join("project"), "session-r");
    let complete = fs::metadata(&path).expect("metadata").len();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append")
        .write_all(b"{\"kind\":\"entry\"")
        .expect("torn tail");
    let bytes = fs::read(&path).expect("read");

    let items = read_all(&bytes, 0);

    assert_eq!(items.last().expect("last").next_offset, complete);
}

/// A complete but undecodable line surfaces as an error item.
#[test]
fn reader_reports_invalid_complete_line() {
    let root = tempfile::tempdir().expect("root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(SystemClock));
    let path =
        write_session(&factory, root.path().join("project"), "session-r");
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append")
        .write_all(b"not json\n")
        .expect("bad line");
    let bytes = fs::read(&path).expect("read");

    let last = MutationReader::new(&bytes, 0).last().expect("items");

    assert!(matches!(
        last,
        Err(StoreError::InvalidSession(_) | StoreError::Json(_))
    ));
}

/// Session files are listed across cwd directories in sorted order, ignoring other files.
#[test]
fn session_paths_are_sorted_jsonl_files() {
    let root = tempfile::tempdir().expect("root");
    let factory = JsonlStoreFactory::new(root.path(), Arc::new(SystemClock));
    let mut expected = Vec::new();
    for (cwd, id) in [
        ("beta", "session-b1"),
        ("alpha", "session-a1"),
        ("beta", "session-b2"),
        ("alpha", "session-a2"),
    ] {
        expected.push(write_session(&factory, root.path().join(cwd), id));
    }
    let stray = expected[0].parent().expect("dir").join("notes.txt");
    fs::write(stray, "not a session").expect("stray");
    expected.sort();

    assert_eq!(factory.session_paths().expect("paths"), expected);
    assert_eq!(factory.sessions_root(), root.path());
}
