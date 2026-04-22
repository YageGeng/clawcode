use kernel::{InMemorySessionStore, SessionId, ThreadId, Turn, TurnContext};
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
        .append_turn_state(session_id.clone(), thread_id.clone(), turn)
        .await
        .unwrap();

    let messages = store
        .load_messages_state(session_id, thread_id, 10)
        .await
        .unwrap();

    assert_eq!(
        messages,
        vec![Message::user("hello"), Message::assistant("hi there")]
    );
}

#[tokio::test]
async fn in_memory_store_exposes_incremental_messages_before_turn_finalizes() {
    let store = InMemorySessionStore::default();
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    store
        .begin_turn_state(
            session_id.clone(),
            thread_id.clone(),
            "hello".to_string(),
            Message::user("hello"),
        )
        .await
        .unwrap();
    store
        .append_message_state(
            session_id.clone(),
            thread_id.clone(),
            Message::assistant("thinking"),
        )
        .await
        .unwrap();

    let interim_messages = store
        .load_messages_state(session_id.clone(), thread_id.clone(), 10)
        .await
        .unwrap();
    assert_eq!(
        interim_messages,
        vec![Message::user("hello"), Message::assistant("thinking")]
    );

    let turn_context = TurnContext::new(session_id.clone(), thread_id.clone());
    store
        .finalize_turn_state(&turn_context, Usage::new())
        .await
        .unwrap();

    let final_messages = store
        .load_messages_state(session_id, thread_id, 10)
        .await
        .unwrap();
    assert_eq!(
        final_messages,
        vec![Message::user("hello"), Message::assistant("thinking")]
    );
}

#[tokio::test]
async fn in_memory_store_take_pending_input_keeps_non_pending_continuations_queued() {
    let store = InMemorySessionStore::default();
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    store
        .queue_continuation(
            session_id.clone(),
            thread_id.clone(),
            kernel::session::SessionContinuationRequest::SystemFollowUp {
                input: "system follow up".to_string(),
            },
        )
        .await;

    let error = store
        .drain_pending_input(session_id.clone(), thread_id.clone())
        .await
        .expect_err("non-pending continuations should not be consumed as pending input");
    assert!(matches!(error, kernel::Error::Runtime { .. }));

    let continuation = store
        .take_continuation_state(session_id, thread_id)
        .await
        .unwrap();
    assert_eq!(
        continuation,
        Some(
            kernel::session::SessionContinuationRequest::SystemFollowUp {
                input: "system follow up".to_string(),
            }
        ),
        "the failed pending-input read should leave the queued continuation intact"
    );
}
