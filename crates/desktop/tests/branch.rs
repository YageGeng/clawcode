//! Finding the session-tree entry behind a transcript message, for Branch
//! (its parent) and Fork (the entry itself).

use desktop::branch::{MessageAuthor, MessageEntries as _};
use protocol::{
    EntryId, LaneId, SessionId, SessionTreeEntry, SessionTreeSnapshot,
    TimestampMs,
};
use serde_json::json;

/// One tree entry of `kind` under `parent` carrying `payload`.
fn entry(
    id: &str,
    parent: Option<&str>,
    kind: &str,
    payload: serde_json::Value,
) -> SessionTreeEntry {
    SessionTreeEntry::builder()
        .entry_id(EntryId::try_from(id).expect("entry"))
        .parent_id(
            parent.map(|parent| EntryId::try_from(parent).expect("parent")),
        )
        .kind(kind.to_string())
        .timestamp_ms(TimestampMs::from(1_000))
        .payload(payload)
        .build()
}

/// A `message` entry payload with this message id and content type.
fn message(message_id: &str, role: &str) -> serde_json::Value {
    json!({ "message_id": message_id, "content": { "type": role, "blocks": [] } })
}

/// A tree with entries in order and the last one as the leaf.
fn tree(entries: Vec<SessionTreeEntry>) -> SessionTreeSnapshot {
    SessionTreeSnapshot::builder()
        .session_id(SessionId::try_from("session-1").expect("session"))
        .lane(LaneId::try_from("main").expect("lane"))
        .leaf_id(entries.last().map(|entry| entry.entry_id.clone()))
        .entries(entries)
        .build()
}

/// The second question's entry hangs below the first answer.
#[test]
fn message_entry_finds_a_user_message_and_its_parent() {
    let tree = tree(vec![
        entry("e1", None, "message", message("u1", "user")),
        entry("e2", Some("e1"), "message", message("a1", "assistant")),
        entry("e3", Some("e2"), "message", message("u2", "user")),
    ]);

    let found = tree
        .message_entry("u2", MessageAuthor::User)
        .expect("entry");
    assert_eq!(found.entry_id.to_string(), "e3");
    assert_eq!(
        found.parent_id.as_ref().map(ToString::to_string).as_deref(),
        Some("e2")
    );
}

/// The first question sits at the root.
#[test]
fn message_entry_of_the_first_message_has_no_parent() {
    let tree = tree(vec![entry("e1", None, "message", message("u1", "user"))]);

    assert_eq!(
        tree.message_entry("u1", MessageAuthor::User)
            .expect("entry")
            .parent_id,
        None
    );
}

/// Other entry kinds, the wrong role, and unknown ids find nothing.
#[test]
fn message_entry_ignores_other_kinds_and_roles() {
    let tree = tree(vec![
        entry("e1", None, "compaction", message("u1", "user")),
        entry("e2", Some("e1"), "message", message("u2", "user")),
    ]);

    assert!(tree.message_entry("u1", MessageAuthor::User).is_none());
    assert!(tree.message_entry("u2", MessageAuthor::Assistant).is_none());
    assert!(tree.message_entry("missing", MessageAuthor::User).is_none());
}
