use kernel::session::{InMemorySessionStore, SessionId, SessionStore, ThreadId, Turn};
use llm::{completion::Message, usage::Usage};

#[tokio::test]
async fn in_memory_store_appends_and_reads_messages() {
    let store = InMemorySessionStore::default();
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    let turn = Turn::new(
        "hello",
        vec![Message::user("hello"), Message::assistant("hi there")],
        Usage::new(),
    );

    store
        .append_turn(session_id.clone(), thread_id.clone(), turn)
        .await
        .unwrap();

    let messages = store
        .load_messages(session_id, thread_id, 10)
        .await
        .unwrap();

    assert_eq!(
        messages,
        vec![Message::user("hello"), Message::assistant("hi there")]
    );
}
