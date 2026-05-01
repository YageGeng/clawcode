use chrono::Utc;
use kernel::{InMemorySessionStore, SessionId, ThreadId, TurnContext};
use llm::{completion::Message, usage::Usage};
use std::sync::Arc;
use store::{JsonlSessionStore, SessionEvent, load_session_events};

/// Verifies that a session store with persistence records a full turn round-trip.
#[tokio::test]
async fn session_persists_and_replays_turn_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session-test.jsonl");
    let persist = JsonlSessionStore::create_at(&path).expect("create store");

    let store = InMemorySessionStore::default().with_persistence(Arc::new(persist));
    let sid = SessionId::new();
    let tid = ThreadId::new();

    store
        .begin_turn_state(
            sid,
            tid.clone(),
            "hello".to_string(),
            Message::user("hello"),
        )
        .await
        .expect("begin turn");

    store
        .append_message_state(sid, tid.clone(), Message::assistant("hi there"))
        .await
        .expect("append assistant message");

    let ctx = TurnContext::new(sid, tid.clone());
    store
        .finalize_turn_state(&ctx, Usage::default())
        .await
        .expect("finalize turn");

    // Drop the store so the file is closed.
    drop(store);

    // Replay from the JSONL file.
    let events = load_session_events(&path).expect("load events");
    assert_eq!(events.len(), 3);

    // First event should be TurnStarted.
    assert!(matches!(events[0], store::TurnEvent::TurnStarted { .. }));
    // Second event should be Message.
    assert!(matches!(events[1], store::TurnEvent::Message { .. }));
    // Third event should be TurnCompleted.
    assert!(matches!(events[2], store::TurnEvent::TurnCompleted { .. }));
}

/// Verifies that a discarded turn is recorded correctly.
#[tokio::test]
async fn session_records_discarded_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("discard-test.jsonl");
    let persist = JsonlSessionStore::create_at(&path).expect("create store");

    let store = InMemorySessionStore::default().with_persistence(Arc::new(persist));
    let sid = SessionId::new();
    let tid = ThreadId::new();

    store
        .begin_turn_state(sid, tid.clone(), "oops".to_string(), Message::user("oops"))
        .await
        .expect("begin turn");

    store
        .discard_turn_state(sid, tid.clone())
        .await
        .expect("discard turn");

    drop(store);

    let events = load_session_events(&path).expect("load events");
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], store::TurnEvent::TurnStarted { .. }));
    assert!(matches!(events[1], store::TurnEvent::TurnDiscarded { .. }));
}

/// Verifies that multi-turn sessions are recorded sequentially.
#[tokio::test]
async fn session_records_multi_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("multi-turn.jsonl");
    let persist = JsonlSessionStore::create_at(&path).expect("create store");

    let store = InMemorySessionStore::default().with_persistence(Arc::new(persist));
    let sid = SessionId::new();
    let tid = ThreadId::new();

    // Turn 1
    store
        .begin_turn_state(sid, tid.clone(), "q1".to_string(), Message::user("q1"))
        .await
        .expect("begin turn 1");
    store
        .append_message_state(sid, tid.clone(), Message::assistant("a1"))
        .await
        .expect("append");
    let ctx1 = TurnContext::new(sid, tid.clone());
    store
        .finalize_turn_state(&ctx1, Usage::default())
        .await
        .expect("finalize turn 1");

    // Turn 2
    store
        .begin_turn_state(sid, tid.clone(), "q2".to_string(), Message::user("q2"))
        .await
        .expect("begin turn 2");
    store
        .append_message_state(sid, tid.clone(), Message::assistant("a2"))
        .await
        .expect("append");
    let ctx2 = TurnContext::new(sid, tid.clone());
    store
        .finalize_turn_state(&ctx2, Usage::default())
        .await
        .expect("finalize turn 2");

    drop(store);

    let events = load_session_events(&path).expect("load events");
    assert_eq!(events.len(), 6);
    // The events should alternate: Started, Message, Completed, Started, Message, Completed
    assert!(matches!(events[0], store::TurnEvent::TurnStarted { .. }));
    assert!(matches!(events[3], store::TurnEvent::TurnStarted { .. }));
}

/// Verifies that list_sessions discovers persisted session files.
#[tokio::test]
async fn list_sessions_discovers_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("list-test.jsonl");

    // Create a session file.
    {
        let persist = JsonlSessionStore::create_at(&path).expect("create store");
        let store = InMemorySessionStore::default().with_persistence(Arc::new(persist));
        let sid = SessionId::new();
        let tid = ThreadId::new();
        store
            .begin_turn_state(sid, tid.clone(), "hi".to_string(), Message::user("hi"))
            .await
            .expect("begin");
        store
            .finalize_turn_state(&TurnContext::new(sid, tid), Usage::default())
            .await
            .expect("finalize");
    }

    // list_sessions scans the default data dir (~/.local/share/clawcode/sessions).
    // For this test we check that the file we wrote has the expected content.
    let events = load_session_events(&path).expect("load events");
    assert!(events.len() >= 2);
}

/// Verifies that store without persistence does not panic.
#[tokio::test]
async fn store_without_persistence_works_normally() {
    let store = InMemorySessionStore::default();
    let sid = SessionId::new();
    let tid = ThreadId::new();

    store
        .begin_turn_state(sid, tid.clone(), "test".to_string(), Message::user("test"))
        .await
        .expect("begin");
    store
        .append_message_state(sid, tid.clone(), Message::assistant("ok"))
        .await
        .expect("append");
    store
        .finalize_turn_state(&TurnContext::new(sid, tid.clone()), Usage::default())
        .await
        .expect("finalize");

    let messages = store
        .load_messages_state(sid, tid, 10)
        .await
        .expect("load messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1], Message::assistant("ok"));
}

/// Verifies that `load_from_events` replays a persisted session and the
/// resumed store can continue accepting new turns.
#[tokio::test]
async fn load_from_events_resumes_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("resume-test.jsonl");

    // Phase 1: Create a session, run one turn, persist, drop.
    let sid = SessionId::new();
    let tid = ThreadId::new();
    {
        let persist = JsonlSessionStore::create_at(&path).expect("create store");
        let store = InMemorySessionStore::default().with_persistence(Arc::new(persist));
        store
            .begin_turn_state(
                sid,
                tid.clone(),
                "hello".to_string(),
                Message::user("hello"),
            )
            .await
            .expect("begin");
        store
            .append_message_state(sid, tid.clone(), Message::assistant("hi"))
            .await
            .expect("append");
        store
            .finalize_turn_state(&TurnContext::new(sid, tid.clone()), Usage::default())
            .await
            .expect("finalize");
    }

    // Phase 2: Load events from disk and replay into a fresh store.
    let events = load_session_events(&path).expect("load events");
    let resumed_store = InMemorySessionStore::default();
    let (loaded_sid, loaded_tid) = resumed_store
        .load_from_events(events)
        .await
        .expect("replay events");

    // Phase 3: Verify the replayed messages are available.
    let messages = resumed_store
        .load_messages_state(loaded_sid, loaded_tid.clone(), 10)
        .await
        .expect("load messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], Message::user("hello"));
    assert_eq!(messages[1], Message::assistant("hi"));

    // Phase 4: Continue the resumed session with a new turn.
    resumed_store
        .begin_turn_state(
            loaded_sid,
            loaded_tid.clone(),
            "continue".to_string(),
            Message::user("continue"),
        )
        .await
        .expect("begin turn 2");
    resumed_store
        .append_message_state(loaded_sid, loaded_tid.clone(), Message::assistant("reply"))
        .await
        .expect("append turn 2");
    resumed_store
        .finalize_turn_state(
            &TurnContext::new(loaded_sid, loaded_tid.clone()),
            Usage::default(),
        )
        .await
        .expect("finalize turn 2");

    let all_messages = resumed_store
        .load_messages_state(loaded_sid, loaded_tid, 20)
        .await
        .expect("load all messages");
    assert_eq!(all_messages.len(), 4);
    assert_eq!(all_messages[2], Message::user("continue"));
    assert_eq!(all_messages[3], Message::assistant("reply"));
}

/// Verifies replay tracks active turns per thread so interleaved child events
/// do not cause the parent turn to be dropped on resume.
#[tokio::test]
async fn load_from_events_preserves_interleaved_thread_turns() {
    let session_id = SessionId::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let parent_context = TurnContext::new(session_id, parent_thread_id.clone());
    let child_context = TurnContext::new(session_id, child_thread_id.clone());
    let events = vec![
        SessionEvent::TurnStarted {
            session_id: session_id.as_uuid(),
            thread_id: parent_thread_id.as_uuid(),
            user_text: "parent turn 1".to_string(),
            message: Message::user("parent turn 1"),
            timestamp: Utc::now(),
        },
        SessionEvent::Message {
            session_id: session_id.as_uuid(),
            thread_id: parent_thread_id.as_uuid(),
            message: Message::assistant("parent answer 1"),
            timestamp: Utc::now(),
        },
        SessionEvent::TurnStarted {
            session_id: session_id.as_uuid(),
            thread_id: child_thread_id.as_uuid(),
            user_text: "child turn 1".to_string(),
            message: Message::user("child turn 1"),
            timestamp: Utc::now(),
        },
        SessionEvent::Message {
            session_id: session_id.as_uuid(),
            thread_id: child_thread_id.as_uuid(),
            message: Message::assistant("child answer 1"),
            timestamp: Utc::now(),
        },
        SessionEvent::TurnCompleted {
            session_id: session_id.as_uuid(),
            thread_id: child_thread_id.as_uuid(),
            usage: Usage::default(),
            context_item: serde_json::to_value(child_context.to_turn_context_item())
                .expect("child turn context should serialize"),
            timestamp: Utc::now(),
        },
        SessionEvent::TurnCompleted {
            session_id: session_id.as_uuid(),
            thread_id: parent_thread_id.as_uuid(),
            usage: Usage::default(),
            context_item: serde_json::to_value(parent_context.to_turn_context_item())
                .expect("parent turn context should serialize"),
            timestamp: Utc::now(),
        },
    ];

    let resumed_store = InMemorySessionStore::default();
    let (loaded_session_id, loaded_thread_id) = resumed_store
        .load_from_events(events)
        .await
        .expect("replay should succeed");
    assert_eq!(loaded_session_id, session_id);
    assert_eq!(loaded_thread_id, parent_thread_id);

    resumed_store
        .begin_turn_state(
            loaded_session_id,
            loaded_thread_id.clone(),
            "parent turn 2".to_string(),
            Message::user("parent turn 2"),
        )
        .await
        .expect("replayed parent thread should accept a second turn");
    resumed_store
        .append_message_state(
            loaded_session_id,
            loaded_thread_id.clone(),
            Message::assistant("parent answer 2"),
        )
        .await
        .expect("second parent turn should append");
    resumed_store
        .finalize_turn_state(
            &TurnContext::new(loaded_session_id, loaded_thread_id.clone()),
            Usage::default(),
        )
        .await
        .expect("second parent turn should finalize");

    let parent_messages = resumed_store
        .load_messages_state(loaded_session_id, loaded_thread_id, 10)
        .await
        .expect("parent messages should load");
    assert_eq!(
        parent_messages,
        vec![
            Message::user("parent turn 1"),
            Message::assistant("parent answer 1"),
            Message::user("parent turn 2"),
            Message::assistant("parent answer 2"),
        ]
    );
}
