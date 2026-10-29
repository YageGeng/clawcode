use std::sync::Arc;

use kernel::{AnswerOutcome, KernelError, WhenBusy};
use protocol::{AgentEventPayload, MessageContent, StopReason};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::fixture::{
    RecordingSinks, Route, TestKernel, finished, request, text,
};

/// Idle submissions publish live events and return the persisted assistant answer.
#[tokio::test]
async fn submit_runs_idle_session_and_returns_answer() {
    let test = TestKernel::new(|_| Route::answer("pong"));
    let session = test.create("owner").await;
    let answer = test
        .kernel
        .submit(request(&session, "ping", WhenBusy::FollowUp))
        .await
        .expect("submit");
    assert_eq!(answer.text, "pong");
    assert!(answer.message_id.is_some());
    assert!(matches!(answer.outcome, AnswerOutcome::Completed));
    assert!(test.sinks.saw(&session, |e| matches!(
        e,
        AgentEventPayload::RunStart { .. }
    )));
}

/// Delegated text cannot execute bash or direct Slash Commands.
#[tokio::test]
async fn submit_treats_bang_and_slash_text_as_plain_message() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    let session = test.create("owner").await;
    for input in ["!echo pwned", "/compact"] {
        test.kernel
            .submit(request(&session, input, WhenBusy::FollowUp))
            .await
            .expect("submit");
    }
    assert_eq!(test.model.last_texts(), ["!echo pwned", "/compact"]);
    assert!(
        test.kernel
            .session_transcript(&session)
            .expect("transcript")
            .iter()
            .all(|m| !matches!(
                m.content,
                MessageContent::BashExecution { .. }
            ))
    );
}

/// A busy submission waits for the run that consumes its queued input.
#[tokio::test]
async fn submit_queues_when_busy_and_answers_after_run() {
    let release = Arc::new(Notify::new());
    let gate = Arc::clone(&release);
    let test = TestKernel::new(move |r| {
        if r.messages
            .last()
            .is_some_and(|m| text(&m.content) == "first")
        {
            Route::Wait(Arc::clone(&gate), vec![finished(StopReason::EndTurn)])
        } else {
            Route::answer("second answer")
        }
    });
    let session = test.create("owner").await;
    let kernel = test.kernel.clone();
    let initial = request(&session, "first", WhenBusy::FollowUp);
    let first = tokio::spawn(async move { kernel.submit(initial).await });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let kernel = test.kernel.clone();
    let queued = request(&session, "second", WhenBusy::FollowUp);
    let second = tokio::spawn(async move { kernel.submit(queued).await });
    test.wait_until(|| {
        !test
            .kernel
            .pending_messages(&session)
            .expect("pending")
            .follow_up
            .is_empty()
    })
    .await;
    release.notify_one();
    assert_eq!(
        second.await.expect("join").expect("answer").text,
        "second answer"
    );
    first.await.expect("join").expect("first answer");
}

/// Inputs queued too late for a cancelled run are rescued into their own run.
#[tokio::test]
async fn submit_rescues_message_left_queued_by_cancelled_run() {
    let test = TestKernel::new(|r| {
        if r.messages
            .last()
            .is_some_and(|m| text(&m.content) == "first")
        {
            Route::Hang
        } else {
            Route::answer("answer for B")
        }
    });
    let session = test.create("owner").await;
    let kernel = test.kernel.clone();
    let initial = request(&session, "first", WhenBusy::FollowUp);
    let first = tokio::spawn(async move { kernel.submit(initial).await });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let kernel = test.kernel.clone();
    let queued = request(&session, "B", WhenBusy::FollowUp);
    let second = tokio::spawn(async move { kernel.submit(queued).await });
    test.wait_until(|| {
        !test
            .kernel
            .pending_messages(&session)
            .expect("pending")
            .follow_up
            .is_empty()
    })
    .await;
    test.kernel.cancel_session(&session).expect("cancel");
    assert_eq!(
        second.await.expect("join").expect("answer").text,
        "answer for B"
    );
    first.await.expect("join").expect("first answer");
}

/// Cancellation that precedes admission cannot be replaced by a fresh run token.
#[tokio::test]
async fn submit_with_cancelled_parent_cancels_run() {
    let test = TestKernel::new(|_| Route::Hang);
    let session = test.create("owner").await;
    let parent = CancellationToken::new();
    parent.cancel();
    let mut input = request(&session, "work", WhenBusy::FollowUp);
    input.cancellation = Some(parent);
    let answer = test.kernel.submit(input).await.expect("submit");
    assert!(matches!(answer.outcome, AnswerOutcome::Cancelled));
}

/// A live subscription must attach while a child waits for user input inside its run.
#[tokio::test]
async fn resume_attaches_to_running_session_without_waiting_for_answer() {
    let test = TestKernel::new(|_| Route::Hang);
    let session = test.create("owner").await;
    let kernel = test.kernel.clone();
    let input = request(&session, "hang", WhenBusy::FollowUp);
    let run = tokio::spawn(async move { kernel.submit(input).await });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let attached = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        test.kernel
            .resume_session(session.clone(), test.root.path().into()),
    )
    .await;
    test.kernel.cancel_session(&session).expect("cancel");
    run.await.expect("join").expect("run");
    attached
        .expect("live attach must not wait for run completion")
        .expect("attach");
    test.kernel.close_session(&session).await.expect("close");
}

/// Replacing event destinations would silently split recovery streams and is rejected.
#[test]
fn installing_event_sinks_twice_fails() {
    let test = TestKernel::new(|_| Route::answer("ok"));
    assert!(matches!(
        test.kernel
            .install_event_sinks(Arc::new(RecordingSinks::default())),
        Err(KernelError::EventSinksInstalled)
    ));
}

/// A queued caller's cancellation waits for child cleanup both before and after consumption.
#[tokio::test]
async fn queued_parent_cancellation_drains_the_answering_run() {
    for consume in [false, true] {
        let release = Arc::new(Notify::new());
        let gate = Arc::clone(&release);
        let test = TestKernel::new(move |request| {
            if request
                .messages
                .last()
                .is_some_and(|message| text(&message.content) == "first")
            {
                Route::Wait(
                    Arc::clone(&gate),
                    vec![finished(StopReason::EndTurn)],
                )
            } else {
                Route::Hang
            }
        });
        let session = test.create("owner").await;
        let kernel = test.kernel.clone();
        let input = request(&session, "first", WhenBusy::FollowUp);
        let first = tokio::spawn(async move { kernel.submit(input).await });
        test.wait_until(|| !test.model.last_texts().is_empty())
            .await;
        let cancellation = CancellationToken::new();
        let mut input = request(&session, "second", WhenBusy::FollowUp);
        input.cancellation = Some(cancellation.clone());
        let kernel = test.kernel.clone();
        let second = tokio::spawn(async move { kernel.submit(input).await });
        test.wait_until(|| {
            !test
                .kernel
                .pending_messages(&session)
                .expect("pending")
                .follow_up
                .is_empty()
        })
        .await;
        if consume {
            release.notify_one();
            test.wait_until(|| {
                test.model.last_texts().contains(&"second".into())
            })
            .await;
        }
        cancellation.cancel();
        assert_eq!(
            second.await.expect("second task").expect("second").outcome,
            AnswerOutcome::Cancelled
        );
        assert!(!test.kernel.session_runtime(&session).expect("idle").running);
        assert_eq!(
            first.await.expect("first task").expect("first").outcome,
            AnswerOutcome::Cancelled
        );
        assert!(
            test.kernel
                .pending_messages(&session)
                .expect("queue cleared")
                .follow_up
                .is_empty()
        );
        test.kernel.close_session(&session).await.expect("close");
    }
}

/// Withdrawing queued delegated input must not return the previous run's unrelated answer.
#[tokio::test]
async fn removed_submission_is_cancelled_without_reporting_an_unrelated_answer()
{
    let release = Arc::new(Notify::new());
    let gate = Arc::clone(&release);
    let test = TestKernel::new(move |_| {
        Route::Wait(
            Arc::clone(&gate),
            vec![
                protocol::ModelStreamEvent::TextDelta(
                    "unrelated answer".into(),
                ),
                finished(StopReason::EndTurn),
            ],
        )
    });
    let session = test.create("owner").await;
    let kernel = test.kernel.clone();
    let input = request(&session, "first", WhenBusy::FollowUp);
    let run = tokio::spawn(async move { kernel.submit(input).await });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let kernel = test.kernel.clone();
    let input = request(&session, "withdraw me", WhenBusy::FollowUp);
    let queued = tokio::spawn(async move { kernel.submit(input).await });
    test.wait_until(|| {
        !test
            .kernel
            .pending_messages(&session)
            .expect("pending")
            .follow_up
            .is_empty()
    })
    .await;
    let queue_id = test
        .kernel
        .pending_messages(&session)
        .expect("pending")
        .follow_up
        .first()
        .expect("queued")
        .queue_id
        .clone();
    test.kernel
        .remove_pending_message(&session, &queue_id)
        .expect("withdraw");
    release.notify_one();
    let answer = queued.await.expect("join").expect("answer");
    assert_eq!(answer.outcome, AnswerOutcome::Cancelled);
    assert!(answer.text.is_empty());
    run.await.expect("join").expect("run");
    test.kernel.close_session(&session).await.expect("close");
}
