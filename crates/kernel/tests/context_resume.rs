use kernel::{
    SessionId, SessionTaskContext, ThreadId,
    context::TurnContext,
    session::{SessionContinuationRequest, Turn},
};
use llm::{completion::Message, usage::Usage};

#[tokio::test]
async fn session_task_context_drains_queued_continuations_in_order() {
    let session = SessionTaskContext::new();
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    session
        .queue_continuation(
            session_id.clone(),
            thread_id.clone(),
            SessionContinuationRequest::PendingInput {
                input: "one".to_string(),
            },
        )
        .await;
    session
        .queue_continuation(
            session_id.clone(),
            thread_id.clone(),
            SessionContinuationRequest::SystemFollowUp {
                input: "two".to_string(),
            },
        )
        .await;

    assert!(matches!(
        session
            .drain_continuation(session_id.clone(), thread_id.clone())
            .await,
        Some(SessionContinuationRequest::PendingInput { input }) if input == "one"
    ));
    assert!(matches!(
        session.drain_continuation(session_id, thread_id).await,
        Some(SessionContinuationRequest::SystemFollowUp { input }) if input == "two"
    ));
}

#[tokio::test]
async fn session_task_context_exposes_history_mutations() {
    let session = SessionTaskContext::new();
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let turn_context =
        TurnContext::new(session_id.clone(), thread_id.clone()).with_system_prompt("system");

    session
        .with_history(session_id.clone(), thread_id.clone(), |history| {
            history.begin_turn("hello".to_string(), Message::user("hello"));
            history.append_message(Message::assistant("world"));
            history.finalize_turn(Usage::new(), &turn_context);
        })
        .await;

    let prompt = session
        .read_history(session_id, thread_id, |history| history.prompt_messages(8))
        .await
        .expect("history should exist after mutation");
    assert_eq!(
        prompt,
        vec![Message::user("hello"), Message::assistant("world")]
    );
}

#[tokio::test]
async fn session_task_context_mirrors_store_style_message_access() {
    let session = SessionTaskContext::new();
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    session
        .append_turn_state(
            session_id.clone(),
            thread_id.clone(),
            Turn::new(
                "hello",
                vec![Message::user("hello"), Message::assistant("hi there")],
                Usage::new(),
            ),
        )
        .await
        .unwrap();

    let messages = session
        .load_messages_state(session_id, thread_id, 10)
        .await
        .unwrap();
    assert_eq!(
        messages,
        vec![Message::user("hello"), Message::assistant("hi there")]
    );
}

#[tokio::test]
async fn session_task_context_take_pending_input_keeps_non_pending_continuations_queued() {
    let session = SessionTaskContext::new();
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();

    session
        .queue_continuation(
            session_id.clone(),
            thread_id.clone(),
            SessionContinuationRequest::SystemFollowUp {
                input: "system follow up".to_string(),
            },
        )
        .await;

    let error = session
        .drain_pending_input(session_id.clone(), thread_id.clone())
        .await
        .expect_err("non-pending continuations should not be consumed as pending input");
    assert!(matches!(error, kernel::Error::Runtime { .. }));

    let continuation = session.drain_continuation(session_id, thread_id).await;
    assert_eq!(
        continuation,
        Some(SessionContinuationRequest::SystemFollowUp {
            input: "system follow up".to_string(),
        }),
        "failed pending-input reads should leave the queued continuation untouched"
    );
}

#[test]
fn context_manager_can_reconstruct_latest_reference_snapshot() {
    let session_id = SessionId::new();
    let thread_id = ThreadId::new();
    let mut history = kernel::ContextManager::new();
    let first = TurnContext::new(session_id.clone(), thread_id.clone()).with_system_prompt("one");
    let second = first.clone().with_timezone("Asia/Shanghai");

    history.begin_turn("a".to_string(), Message::user("a"));
    history.finalize_turn(Usage::new(), &first);
    history.begin_turn("b".to_string(), Message::user("b"));
    history.finalize_turn(Usage::new(), &second);

    let rebuilt =
        kernel::ContextManager::reconstruct_from_completed_turns(history.completed_turns());
    assert_eq!(
        rebuilt.reference_context_item(),
        Some(second.to_turn_context_item())
    );
}
