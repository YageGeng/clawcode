//! Mailbox delivery: idle writes, wakes, turn-boundary drains, and run-close write-back.

use std::sync::Arc;
use std::time::Duration;

use kernel::MailPlacement;
use protocol::{
    MailKind, MessageContent, ModelStreamEvent, StopReason, ToolCall,
    ToolCallId,
};
use tokio::sync::Notify;

use super::fixture::{KernelPolicy, Route, TestKernel, finished, text};

/// Scripts a provider answer that waits for the test before completing.
fn held(gate: &Arc<Notify>, answer: &str) -> Route {
    Route::Wait(
        Arc::clone(gate),
        vec![
            ModelStreamEvent::TextDelta(answer.into()),
            finished(StopReason::EndTurn),
        ],
    )
}

/// Scripts a held provider turn that asks for one side-effect-free tool call.
fn held_tool_call(gate: &Arc<Notify>) -> Route {
    Route::Wait(
        Arc::clone(gate),
        vec![
            ModelStreamEvent::ToolCall(
                ToolCall::builder()
                    .tool_call_id(
                        ToolCallId::try_from("call-todo").expect("id"),
                    )
                    .name(protocol::TODO_WRITE_TOOL.into())
                    .arguments(serde_json::json!({ "todos": [] }))
                    .build(),
            ),
            finished(StopReason::ToolUse),
        ],
    )
}

/// Returns the last message text of one provider request.
fn last_text(test: &TestKernel, index: usize) -> String {
    test.model
        .last_texts()
        .get(index)
        .cloned()
        .unwrap_or_default()
}

/// A queue-only mail to an idle Session is persisted and starts no Run.
#[tokio::test]
async fn mail_to_idle_session_is_written_without_a_run() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("idle").await;
    let placement = test
        .mail(&session, "/root/a", MailKind::Message, "hello")
        .await;
    assert_eq!(placement, MailPlacement::Written);
    assert_eq!(
        test.agent_mails(&session),
        vec![
            "Message Type: message\nTask name: /root\nSender: /root/a\nPayload:\nhello"
        ]
    );
    assert_eq!(test.model.request_count(), 0);
    assert!(
        !test
            .kernel
            .session_runtime(&session)
            .expect("runtime")
            .running
    );
}

/// A new task wakes an idle Session whose Run starts from the mail itself.
#[tokio::test]
async fn new_task_to_idle_session_wakes_a_run() {
    let test = TestKernel::new(|_| Route::answer("done"));
    let session = test.create("wake").await;
    let placement = test
        .mail(&session, "/root/a", MailKind::NewTask, "Survey.")
        .await;
    assert_eq!(placement, MailPlacement::Woken);
    test.wait_until(|| test.model.request_count() == 1).await;
    assert!(last_text(&test, 0).starts_with("Message Type: new_task\n"));
    test.wait_until(|| {
        !test
            .kernel
            .session_runtime(&session)
            .expect("runtime")
            .running
    })
    .await;
    let transcript = test
        .kernel
        .session_transcript(&session)
        .expect("transcript");
    assert!(matches!(&transcript.first().expect("first").content,
            MessageContent::Extension { extension }
                if extension.custom_type == protocol::AGENT_MESSAGE_TYPE));
    assert!(
        !transcript
            .iter()
            .any(|m| matches!(m.content, MessageContent::User { .. }))
    );
}

/// Queue-only mail to a finishing Run is written after its answer without another Turn.
#[tokio::test]
async fn queue_only_mail_does_not_extend_a_finishing_run() {
    let gate = Arc::new(Notify::new());
    let route_gate = Arc::clone(&gate);
    let test = TestKernel::new(move |_| held(&route_gate, "answer"));
    let session = test.create("busy").await;
    let run = test.start(&session, "start");
    test.wait_until(|| test.model.request_count() == 1).await;
    let placement = test
        .mail(&session, "/root/a", MailKind::Message, "later")
        .await;
    assert_eq!(placement, MailPlacement::Queued);
    gate.notify_one();
    run.await.expect("join").expect("run");
    assert_eq!(test.model.request_count(), 1);
    let transcript = test
        .kernel
        .session_transcript(&session)
        .expect("transcript");
    let last = transcript.last().expect("last");
    assert!(text(&last.content).ends_with("Payload:\nlater"));
    let previous = transcript.get(transcript.len() - 2).expect("answer");
    assert!(matches!(previous.content, MessageContent::Assistant { .. }));
}

/// Mail queued during a tool Turn reaches the very next provider request.
#[tokio::test]
async fn mail_enters_the_next_request() {
    let gate = Arc::new(Notify::new());
    let route_gate = Arc::clone(&gate);
    let test = TestKernel::new(move |request| {
        match request.messages.last().map(|m| text(&m.content)).as_deref() {
            Some("start") => held_tool_call(&route_gate),
            _ => Route::answer("done"),
        }
    });
    let session = test.create("tools").await;
    let run = test.start(&session, "start");
    test.wait_until(|| test.model.request_count() == 1).await;
    test.mail(&session, "/root/a", MailKind::Message, "context")
        .await;
    gate.notify_one();
    run.await.expect("join").expect("run");
    assert_eq!(test.model.request_count(), 2);
    assert!(last_text(&test, 1).ends_with("Payload:\ncontext"));
}

/// A new task queued while the model finishes still gets its own Turn.
#[tokio::test]
async fn new_task_extends_a_finishing_run() {
    let gate = Arc::new(Notify::new());
    let route_gate = Arc::clone(&gate);
    let test = TestKernel::new(move |request| {
        match request.messages.last().map(|m| text(&m.content)).as_deref() {
            Some("start") => held(&route_gate, "first"),
            _ => Route::answer("second"),
        }
    });
    let session = test.create("extend").await;
    let run = test.start(&session, "start");
    test.wait_until(|| test.model.request_count() == 1).await;
    let placement = test
        .mail(&session, "/root/a", MailKind::NewTask, "More.")
        .await;
    assert_eq!(placement, MailPlacement::Queued);
    gate.notify_one();
    let result = run.await.expect("join").expect("run");
    // The same Run takes the extra Turn; no second Run is woken for it.
    assert_eq!(result.turns.len(), 2);
    assert_eq!(test.model.request_count(), 2);
    assert!(last_text(&test, 1).starts_with("Message Type: new_task\n"));
}

/// Cancelling and clearing never drop mail, and a cancelled new task does not rewake.
#[tokio::test]
async fn mail_survives_cancel_and_clear() {
    let test = TestKernel::new(|_| Route::Hang);
    let session = test.create("cancel").await;
    let run = test.start(&session, "start");
    test.wait_until(|| test.model.request_count() == 1).await;
    test.mail(&session, "/root/a", MailKind::Message, "note")
        .await;
    test.mail(&session, "/root/a", MailKind::NewTask, "task")
        .await;
    test.kernel.clear_pending_messages(&session).expect("clear");
    test.kernel.cancel_session(&session).expect("cancel");
    let _ = run.await.expect("join");
    let mails = test.agent_mails(&session);
    assert_eq!(mails.len(), 2, "{mails:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(test.model.request_count(), 1);
    let pending = test.kernel.pending_messages(&session).expect("pending");
    assert!(pending.steering.is_empty() && pending.follow_up.is_empty());
}

/// A new task left behind by a Run that hit its Turn limit wakes the next Run once.
#[tokio::test]
async fn leftover_new_task_wakes_after_turn_limit() {
    let gate = Arc::new(Notify::new());
    let route_gate = Arc::clone(&gate);
    let test = TestKernel::with_policy(
        move |request| match request
            .messages
            .last()
            .map(|m| text(&m.content))
            .as_deref()
        {
            Some("start") => held(&route_gate, "first"),
            _ => Route::answer("handled"),
        },
        KernelPolicy {
            max_turns: config::TurnLimit::Limited {
                turns: std::num::NonZeroUsize::new(1).expect("one"),
            },
            ..KernelPolicy::default()
        },
    );
    let session = test.create("limited").await;
    let run = test.start(&session, "start");
    test.wait_until(|| test.model.request_count() == 1).await;
    test.mail(&session, "/root/a", MailKind::NewTask, "follow")
        .await;
    gate.notify_one();
    let _ = run.await.expect("join");
    test.wait_until(|| test.model.request_count() == 2).await;
    assert!(last_text(&test, 1).starts_with("Message Type: new_task\n"));
    test.wait_until(|| {
        !test
            .kernel
            .session_runtime(&session)
            .expect("runtime")
            .running
    })
    .await;
    assert_eq!(test.agent_mails(&session).len(), 1);
}

/// Several new tasks left behind at Run close wake one Run that reads them all.
#[tokio::test]
async fn leftover_new_tasks_wake_one_run() {
    let gate = Arc::new(Notify::new());
    let route_gate = Arc::clone(&gate);
    let test = TestKernel::with_policy(
        move |request| match request
            .messages
            .last()
            .map(|m| text(&m.content))
            .as_deref()
        {
            Some("start") => held(&route_gate, "first"),
            _ => Route::answer("handled"),
        },
        KernelPolicy {
            max_turns: config::TurnLimit::Limited {
                turns: std::num::NonZeroUsize::new(1).expect("one"),
            },
            ..KernelPolicy::default()
        },
    );
    let session = test.create("limited").await;
    let run = test.start(&session, "start");
    test.wait_until(|| test.model.request_count() == 1).await;
    test.mail(&session, "/root/a", MailKind::NewTask, "follow-a")
        .await;
    test.mail(&session, "/root/a", MailKind::NewTask, "follow-b")
        .await;
    gate.notify_one();
    let _ = run.await.expect("join");
    test.wait_until(|| {
        test.agent_mails(&session).len() == 2
            && !test
                .kernel
                .session_runtime(&session)
                .expect("runtime")
                .running
    })
    .await;
    assert_eq!(test.model.request_count(), 2, "one woken Run");
    let mails = test.agent_mails(&session);
    assert!(mails[0].ends_with("follow-a") && mails[1].ends_with("follow-b"));
    assert!(last_text(&test, 1).ends_with("follow-b"));
}

/// Mail text never dispatches user bash or Slash Commands.
#[tokio::test]
async fn mail_input_never_dispatches_commands() {
    let test = TestKernel::new(|_| Route::answer("done"));
    let session = test.create("commands").await;
    for payload in ["!echo x", "/compact"] {
        test.mail(&session, "/root/a", MailKind::NewTask, payload)
            .await;
        test.wait_until(|| {
            !test
                .kernel
                .session_runtime(&session)
                .expect("runtime")
                .running
                && test
                    .agent_mails(&session)
                    .iter()
                    .any(|m| m.ends_with(payload))
        })
        .await;
    }
    let transcript = test
        .kernel
        .session_transcript(&session)
        .expect("transcript");
    assert!(!transcript.iter().any(|m| matches!(
        m.content,
        MessageContent::BashExecution { .. }
            | MessageContent::SlashCommand { .. }
    )));
}

/// Oversized payloads are bounded before they reach the recipient's context.
#[tokio::test]
async fn oversized_mail_is_truncated() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("large").await;
    let payload = "x".repeat(300_000);
    test.mail(&session, "/root/a", MailKind::Message, &payload)
        .await;
    let mail = test.agent_mails(&session).pop().expect("mail");
    assert!(mail.len() <= tools::TOOL_TEXT_BYTES + 200, "{}", mail.len());
    assert!(mail.ends_with("[message truncated]"));
    // A single oversized line keeps its leading bytes instead of vanishing.
    assert!(mail.contains(&"x".repeat(10_000)), "payload dropped");
}
