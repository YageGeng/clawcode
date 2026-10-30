use kernel::{KernelError, SessionEventSinks};
use protocol::{QueueId, QueueKind, RunRequest, TraceId};

use super::fixture::{Route, TestKernel};

/// Removing an absent item is idempotent, including items already claimed by a Run.
#[tokio::test]
async fn removing_missing_queue_item_is_a_no_op() {
    let test = TestKernel::new(|_| Route::answer("done"));
    let session = test.create("queue-remove").await;
    test.kernel
        .remove_pending_message(
            &session,
            &QueueId::try_from("queue-missing").unwrap(),
        )
        .unwrap();
    assert!(
        test.kernel
            .pending_messages(&session)
            .unwrap()
            .follow_up
            .is_empty()
    );
}

/// A settled Run cannot durably accept an item that no consumer will observe.
#[tokio::test]
async fn enqueue_after_run_settles_is_rejected() {
    let test = TestKernel::new(|_| Route::answer("done"));
    let session = test.create("queue-closed").await;
    test.kernel
        .run_traced(
            RunRequest {
                session_id: session.clone(),
                input: "hello".into(),
            },
            test.sinks.sink(&session),
            TraceId::try_from("trace-queue").unwrap(),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        test.kernel
            .queue_message(&session, QueueKind::FollowUp, "late")
            .await,
        Err(KernelError::SessionNotRunning(_))
    ));
    assert_eq!(test.user_texts(&session), ["hello"]);
    assert!(
        test.kernel
            .pending_messages(&session)
            .unwrap()
            .follow_up
            .is_empty()
    );
}

/// Ticket order survives reverse task scheduling and stop returns live queued inputs.
#[tokio::test]
async fn admission_order_and_stop_preserve_each_input_outcome() {
    use kernel::{InsertionNotice, PromptOutcome, PromptRequest};
    let test = TestKernel::new(|_| Route::Hang);
    let session = test.create("queue-order").await;
    let kernel = test.kernel.clone();
    let sink = test.sinks.sink(&session);
    let first = session.clone();
    let run = tokio::spawn(async move {
        kernel
            .run_traced(
                RunRequest {
                    session_id: first,
                    input: "hold".into(),
                },
                sink,
                TraceId::try_from("trace-hold").unwrap(),
                None,
            )
            .await
    });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let a = test.kernel.admission_ticket(&session).unwrap();
    let b = test.kernel.admission_ticket(&session).unwrap();
    let (notice_a, receiver_a) = InsertionNotice::new();
    let (notice_b, receiver_b) = InsertionNotice::new();
    let make = |input: &str, ticket, notice| {
        PromptRequest::builder()
            .run(RunRequest {
                session_id: session.clone(),
                input: input.into(),
            })
            .sink(test.sinks.sink(&session))
            .trace_id(TraceId::try_from("trace-order").unwrap())
            .ticket(ticket)
            .insertion(notice)
            .build()
    };
    let request_b = make("b", b, notice_b);
    let kernel = test.kernel.clone();
    let prompt_b = tokio::spawn(async move { kernel.prompt(request_b).await });
    assert!(matches!(
        test.kernel.prompt(make("a", a, notice_a)).await.unwrap(),
        PromptOutcome::Queued(_)
    ));
    prompt_b.await.unwrap().unwrap();
    let pending = test.kernel.pending_messages(&session).unwrap();
    assert_eq!(
        pending
            .follow_up
            .iter()
            .map(|q| super::fixture::text(&q.message.content))
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    test.kernel.cancel_session(&session).unwrap();
    assert!(matches!(
        receiver_a.await.unwrap(),
        Err(KernelError::PromptCancelled)
    ));
    assert!(matches!(
        receiver_b.await.unwrap(),
        Err(KernelError::PromptCancelled)
    ));
    run.await.unwrap().unwrap();
    assert_eq!(test.user_texts(&session), ["hold"]);
    assert!(
        test.kernel
            .pending_messages(&session)
            .unwrap()
            .follow_up
            .is_empty()
    );
}

/// Promotion preserves identity, moves to steering, and is replayed once after reopen.
#[tokio::test]
async fn promotion_is_one_durable_move() {
    let test = TestKernel::new(|_| Route::Hang);
    let session = test.create("queue-promote").await;
    let kernel = test.kernel.clone();
    let first = session.clone();
    let sink = test.sinks.sink(&session);
    let run = tokio::spawn(async move {
        kernel
            .run_traced(
                RunRequest {
                    session_id: first,
                    input: "hold".into(),
                },
                sink,
                TraceId::try_from("trace-promote").unwrap(),
                None,
            )
            .await
    });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let a = test
        .kernel
        .queue_message(&session, QueueKind::FollowUp, "a")
        .await
        .unwrap();
    let b = test
        .kernel
        .queue_message(&session, QueueKind::FollowUp, "b")
        .await
        .unwrap();
    assert!(
        test.kernel
            .promote_pending_message(&session, &b.queue_id)
            .unwrap()
    );
    assert!(
        test.kernel
            .promote_pending_message(&session, &b.queue_id)
            .unwrap()
    );
    let pending = test.kernel.pending_messages(&session).unwrap();
    assert_eq!(pending.steering[0].message.identity, b.message.identity);
    assert_eq!(pending.follow_up[0].queue_id, a.queue_id);
    test.kernel.cancel_session(&session).unwrap();
    run.await.unwrap().unwrap();
    test.wait_until(|| test.sinks.saw(&session, |event| matches!(event, protocol::AgentEventPayload::QueueChanged { pending } if pending.steering.len() == 1 && pending.follow_up.len() == 1))).await;
    test.kernel.close_session(&session).await.unwrap();
}

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, InputPoint,
    MessageStartPoint,
};
use kernel::{InsertionNotice, PromptRequest, WhenBusy};
use protocol::{InputEvent, InputResult, MessageStartEvent, StopReason};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

impl TestKernel {
    /// Starts a production Run independently of the task driving queue mutations.
    fn start_run(
        &self,
        session: &protocol::SessionId,
        input: &str,
    ) -> tokio::task::JoinHandle<Result<protocol::RunResult, KernelError>> {
        let kernel = self.kernel.clone();
        let sink = self.sinks.sink(session);
        let request = RunRequest {
            session_id: session.clone(),
            input: input.into(),
        };
        tokio::spawn(async move {
            kernel
                .run_traced(
                    request,
                    sink,
                    TraceId::try_from("trace-queue-test").expect("trace id"),
                    None,
                )
                .await
        })
    }

    /// Allocates a real ingress ticket and optional explicit scheduling lane.
    fn client_prompt(
        &self,
        session: &protocol::SessionId,
        input: &str,
        placement: Option<WhenBusy>,
    ) -> (PromptRequest, kernel::InsertionReceiver) {
        let (notice, receiver) = InsertionNotice::new();
        (
            PromptRequest::builder()
                .run(RunRequest {
                    session_id: session.clone(),
                    input: input.into(),
                })
                .sink(self.sinks.sink(session))
                .trace_id(
                    TraceId::try_from("trace-prompt-test").expect("trace id"),
                )
                .ticket(
                    self.kernel
                        .admission_ticket(session)
                        .expect("admission ticket"),
                )
                .insertion(notice)
                .when_busy(placement)
                .submission_id(Some(format!("submission-{input}")))
                .build(),
            receiver,
        )
    }
}

/// Lets an initial model Turn finish while later Turns answer immediately.
fn hold_route(
    release: &Arc<Notify>,
    request: &protocol::ModelRequest,
) -> Route {
    if request
        .messages
        .last()
        .is_some_and(|m| super::fixture::text(&m.content) == "hold")
    {
        Route::Wait(
            Arc::clone(release),
            vec![super::fixture::finished(StopReason::EndTurn)],
        )
    } else {
        Route::answer("answer")
    }
}

/// Claims must be restored both before the next Turn starts and when its preflight fails.
#[tokio::test]
async fn unconsumed_claim_returns_after_turn_limit_or_preflight_failure() {
    for preflight in [false, true] {
        let release = Arc::new(Notify::new());
        let wait = Arc::clone(&release);
        let limit = if preflight {
            config::TurnLimit::Unlimited
        } else {
            config::TurnLimit::Limited {
                turns: std::num::NonZeroUsize::new(1).unwrap(),
            }
        };
        let test = TestKernel::with_policy(
            move |request| hold_route(&wait, request),
            limit,
            WhenBusy::FollowUp,
        );
        let session = test.create("queue-claim-exit").await;
        let run = test.start_run(&session, "hold");
        test.wait_until(|| !test.model.last_texts().is_empty())
            .await;
        let item = test
            .kernel
            .queue_message(&session, QueueKind::FollowUp, "q")
            .await
            .unwrap();
        if preflight {
            *test.model.preflight_failure.lock().unwrap() = Some(|| {
                kernel::ModelError::Unavailable("next turn unavailable".into())
            });
        }
        release.notify_one();
        run.await.expect("join").expect_err("Run must fail");
        assert_eq!(test.user_texts(&session), ["hold"]);
        assert_eq!(
            test.kernel.pending_messages(&session).unwrap().follow_up[0]
                .queue_id,
            item.queue_id
        );
    }
}

/// A live claim is invisible to mutation APIs until it is inserted or returned.
#[derive(Clone)]
struct HoldClaim {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
#[async_trait]
impl ExtensionHandler<MessageStartPoint> for HoldClaim {
    /// Blocks only the queued input, leaving the initial user message and assistant untouched.
    async fn handle(
        &self,
        event: &MessageStartEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        if super::fixture::text(&event.message.content) == "q" {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// Stop returns a listening claim, preserves an orphan, and remove/promote cannot steal a claim.
#[tokio::test]
async fn claimed_input_cancellation_and_mutation_respect_ownership() {
    for mode in ["live", "orphan", "insert"] {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let hook = HoldClaim {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        };
        let first_release = Arc::new(Notify::new());
        let wait = Arc::clone(&first_release);
        let test = TestKernel::with_extensions(
            move |request| hold_route(&wait, request),
            super::insertion::hooks(move |registrar| {
                registrar.on::<MessageStartPoint, _>(hook.clone())
            }),
        );
        let session = test.create("queue-claimed").await;
        let run = test.start_run(&session, "hold");
        test.wait_until(|| !test.model.last_texts().is_empty())
            .await;
        let (request, receiver) = test.client_prompt(&session, "q", None);
        test.kernel.prompt(request).await.unwrap();
        let item = test.kernel.pending_messages(&session).unwrap().follow_up[0]
            .clone();
        let mut receiver = Some(receiver);
        if mode == "orphan" {
            drop(receiver.take());
        }
        first_release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        assert!(
            test.kernel
                .pending_messages(&session)
                .unwrap()
                .follow_up
                .is_empty()
        );
        test.kernel
            .remove_pending_message(&session, &item.queue_id)
            .unwrap();
        assert!(
            !test
                .kernel
                .promote_pending_message(&session, &item.queue_id)
                .unwrap()
        );
        if mode != "insert" {
            test.kernel.cancel_session(&session).unwrap();
        }
        release.notify_one();
        let result = run.await.unwrap();
        if mode == "insert" {
            result.unwrap();
            assert_eq!(
                receiver.take().unwrap().await.unwrap().unwrap(),
                item.message.identity.message_id
            );
            assert_eq!(test.user_texts(&session), ["hold", "q"]);
        } else {
            result.expect_err("Run must fail");
            assert_eq!(test.user_texts(&session), ["hold"]);
            if let Some(receiver) = receiver {
                assert!(matches!(
                    receiver.await.unwrap(),
                    Err(KernelError::PromptCancelled)
                ));
            }
            assert_eq!(
                test.kernel
                    .pending_messages(&session)
                    .unwrap()
                    .follow_up
                    .len(),
                usize::from(mode == "orphan")
            );
        }
    }
}

/// Busy and idle transforms are model input, never a second shell or command ingress.
#[derive(Clone)]
struct TransformInput(Arc<std::sync::atomic::AtomicUsize>);
#[async_trait]
impl ExtensionHandler<InputPoint> for TransformInput {
    /// Rewrites ordinary text into command-looking strings and handles one input locally.
    async fn handle(
        &self,
        event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        if event.text == "hold" {
            return Ok(InputResult::Continue);
        }
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(match event.text.as_str() {
            "shell" => InputResult::Transform {
                text: "!echo must-stay-literal".into(),
            },
            "command" => InputResult::Transform {
                text: "/compact".into(),
            },
            "handled" => InputResult::Handled,
            _ => InputResult::Continue,
        })
    }
}

/// Each prompt runs its input hook exactly once on queue, fallback, and Handled paths.
#[tokio::test]
async fn transformed_input_is_processed_once_and_never_dispatches_commands() {
    for busy in [false, true] {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook = TransformInput(Arc::clone(&calls));
        let release = Arc::new(Notify::new());
        let wait = Arc::clone(&release);
        let test = TestKernel::with_extensions(
            move |request| hold_route(&wait, request),
            super::insertion::hooks(move |registrar| {
                registrar.on::<InputPoint, _>(hook.clone())
            }),
        );
        let session = test.create("queue-transform").await;
        let run = busy.then(|| test.start_run(&session, "hold"));
        if busy {
            test.wait_until(|| !test.model.last_texts().is_empty())
                .await;
        }
        let mut receivers = Vec::new();
        for input in ["shell", "command", "handled"] {
            let (request, receiver) = test.client_prompt(&session, input, None);
            let outcome = test.kernel.prompt(request).await.unwrap();
            if input == "handled" {
                assert!(matches!(outcome, kernel::PromptOutcome::Handled));
                receiver.await.unwrap().unwrap();
            } else {
                receivers.push(receiver);
            }
        }
        release.notify_one();
        if let Some(run) = run {
            run.await.unwrap().unwrap();
        }
        for receiver in receivers {
            receiver.await.unwrap().unwrap();
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        let texts = test.user_texts(&session);
        assert!(texts.iter().any(|text| text == "!echo must-stay-literal"));
        assert!(texts.iter().any(|text| text == "/compact"));
        assert!(!texts.iter().any(|text| text == "handled"));
        assert!(!test.sinks.saw(&session, |event| matches!(
            event,
            protocol::AgentEventPayload::UserBashStarted { .. }
                | protocol::AgentEventPayload::SlashCommandStart { .. }
        )));
    }
}

/// Config defaults determine placement, while explicit steer overtakes older follow-ups.
#[tokio::test]
async fn configured_and_explicit_placement_determine_insertion_order() {
    for default in [WhenBusy::FollowUp, WhenBusy::Steer] {
        let release = Arc::new(Notify::new());
        let wait = Arc::clone(&release);
        let test = TestKernel::with_policy(
            move |request| hold_route(&wait, request),
            config::TurnLimit::Unlimited,
            default,
        );
        let session = test.create("queue-placement").await;
        let run = test.start_run(&session, "hold");
        test.wait_until(|| !test.model.last_texts().is_empty())
            .await;
        let (request, default_rx) =
            test.client_prompt(&session, "default", None);
        test.kernel.prompt(request).await.unwrap();
        let pending = test.kernel.pending_messages(&session).unwrap();
        assert_eq!(
            pending.steering.len(),
            usize::from(matches!(default, WhenBusy::Steer))
        );
        let (request, follow_rx) =
            test.client_prompt(&session, "follow", Some(WhenBusy::FollowUp));
        test.kernel.prompt(request).await.unwrap();
        let (request, steer_rx) =
            test.client_prompt(&session, "steer", Some(WhenBusy::Steer));
        test.kernel.prompt(request).await.unwrap();
        release.notify_one();
        run.await.unwrap().unwrap();
        default_rx.await.unwrap().unwrap();
        follow_rx.await.unwrap().unwrap();
        steer_rx.await.unwrap().unwrap();
        assert_eq!(
            test.user_texts(&session),
            if matches!(default, WhenBusy::Steer) {
                vec!["hold", "default", "steer", "follow"]
            } else {
                vec!["hold", "steer", "default", "follow"]
            }
        );
    }
}

/// An appended enqueue with failed sync is unknown; resume recovers it once without returning input.
#[tokio::test]
async fn queue_write_failures_preserve_uncertain_outcomes_and_recover_without_duplicates()
 {
    use std::sync::atomic::{AtomicBool, Ordering};
    for consumed in [false, true] {
        let fail_sync = Arc::new(AtomicBool::new(false));
        let fail_consumed = Arc::new(AtomicBool::new(false));
        let sync = Arc::clone(&fail_sync);
        let consumption = Arc::clone(&fail_consumed);
        let release = Arc::new(Notify::new());
        let wait = Arc::clone(&release);
        let test = TestKernel::with_store(
            move |request| hold_route(&wait, request),
            move |root, clock| {
                Arc::new(super::failing_queue_store::FailingQueueStoreFactory {
                    inner: store::JsonlStoreFactory::new(
                        root.join("sessions"),
                        clock,
                    ),
                    fail_sync: sync,
                    fail_consumed: consumption,
                })
            },
        );
        let session = test.create("queue-uncertain").await;
        let run = test.start_run(&session, "hold");
        test.wait_until(|| !test.model.last_texts().is_empty())
            .await;
        if !consumed {
            fail_sync.store(true, Ordering::SeqCst);
        }
        let (request, receiver) = test.client_prompt(&session, "q", None);
        let outcome = test.kernel.prompt(request).await;
        if consumed {
            outcome.unwrap();
            let (request, second) = test.client_prompt(&session, "r", None);
            test.kernel.prompt(request).await.unwrap();
            fail_consumed.store(true, Ordering::SeqCst);
            release.notify_one();
            assert!(matches!(
                run.await.unwrap(),
                Err(KernelError::SessionPersistenceUncertain)
            ));
            assert!(matches!(
                second.await.unwrap(),
                Err(KernelError::SessionPersistenceUncertain)
            ));
        } else {
            assert!(matches!(
                outcome,
                Err(KernelError::SessionPersistenceUncertain)
            ));
            assert!(
                test.kernel
                    .pending_messages(&session)
                    .unwrap()
                    .follow_up
                    .is_empty()
            );
            assert!(matches!(
                test.kernel
                    .queue_message(&session, QueueKind::FollowUp, "blocked")
                    .await,
                Err(KernelError::SessionPersistenceUncertain)
            ));
            test.kernel.cancel_session(&session).unwrap();
            run.await.expect("join").expect_err("Run must fail");
        }
        assert!(matches!(
            receiver.await.unwrap(),
            Err(KernelError::SessionPersistenceUncertain)
        ));
        test.kernel.close_session(&session).await.unwrap();
        test.kernel
            .resume_session(session.clone(), test.root.path().into())
            .await
            .unwrap();
        let pending = test.kernel.pending_messages(&session).unwrap();
        assert_eq!(pending.follow_up.len(), 1);
        assert_eq!(
            super::fixture::text(&pending.follow_up[0].message.content),
            if consumed { "r" } else { "q" }
        );
        assert_eq!(
            test.user_texts(&session)
                .iter()
                .filter(|text| *text == "q")
                .count(),
            usize::from(consumed)
        );
        test.start_run(&session, "next").await.unwrap().unwrap();
        assert_eq!(
            test.user_texts(&session)
                .iter()
                .filter(|text| *text == "q")
                .count(),
            1
        );
    }
}

/// Input hooks remain cancellable before either queueing or fallback Run admission.
#[derive(Clone)]
struct HoldInput {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
#[async_trait]
impl ExtensionHandler<InputPoint> for HoldInput {
    /// Pauses one ordinary client input before returning Continue.
    async fn handle(
        &self,
        event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        if event.text == "pause" {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(InputResult::Continue)
    }
}

/// Stop during the input hook rejects the original cancellation generation even after fallback.
#[tokio::test]
async fn stop_during_input_hook_rejects_idle_and_busy_prompts() {
    for busy in [false, true] {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let hook = HoldInput {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        };
        let test = TestKernel::with_extensions(
            |_| Route::Hang,
            super::insertion::hooks(move |r| {
                r.on::<InputPoint, _>(hook.clone())
            }),
        );
        let session = test.create("queue-hook-cancel").await;
        let run = busy.then(|| test.start_run(&session, "hold"));
        if busy {
            test.wait_until(|| !test.model.last_texts().is_empty())
                .await;
        }
        let (request, receiver) = test.client_prompt(&session, "pause", None);
        let kernel = test.kernel.clone();
        let prompt = tokio::spawn(async move { kernel.prompt(request).await });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        test.kernel.cancel_session(&session).unwrap();
        release.notify_one();
        assert!(matches!(
            prompt.await.unwrap(),
            Err(KernelError::PromptCancelled)
        ));
        assert!(matches!(
            receiver.await.unwrap(),
            Err(KernelError::PromptCancelled)
        ));
        if let Some(run) = run {
            run.await.unwrap().unwrap();
        }
        assert!(!test.user_texts(&session).contains(&"pause".into()));
        assert!(
            test.kernel
                .pending_messages(&session)
                .unwrap()
                .follow_up
                .is_empty()
        );
    }
}

/// Direct commands retain admission while waiting; abandoned tickets and stop cannot stall successors.
#[tokio::test]
async fn direct_commands_hold_admission_and_cancelled_tickets_do_not_stall() {
    for command in ["!echo test", "/session"] {
        let release = Arc::new(Notify::new());
        let wait = Arc::clone(&release);
        let test = TestKernel::new(move |request| hold_route(&wait, request));
        let session = test.create("queue-command-order").await;
        let run = test.start_run(&session, "hold");
        test.wait_until(|| !test.model.last_texts().is_empty())
            .await;
        let (request, command_rx) = test.client_prompt(&session, command, None);
        let kernel = test.kernel.clone();
        let cmd = tokio::spawn(async move { kernel.prompt(request).await });
        drop(test.kernel.admission_ticket(&session).unwrap());
        let (request, after_rx) = test.client_prompt(&session, "after", None);
        let kernel = test.kernel.clone();
        let after = tokio::spawn(async move { kernel.prompt(request).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!cmd.is_finished() && !after.is_finished());
        assert!(
            test.kernel
                .pending_messages(&session)
                .unwrap()
                .follow_up
                .is_empty()
        );
        test.kernel.cancel_session(&session).unwrap();
        release.notify_one();
        run.await.unwrap().unwrap();
        assert!(matches!(
            cmd.await.unwrap(),
            Err(KernelError::PromptCancelled)
        ));
        assert!(matches!(
            after.await.unwrap(),
            Err(KernelError::PromptCancelled)
        ));
        assert!(matches!(
            command_rx.await.unwrap(),
            Err(KernelError::PromptCancelled)
        ));
        assert!(matches!(
            after_rx.await.unwrap(),
            Err(KernelError::PromptCancelled)
        ));
        let (next, rx) = test.client_prompt(&session, "next", None);
        tokio::time::timeout(Duration::from_secs(5), test.kernel.prompt(next))
            .await
            .unwrap()
            .unwrap();
        rx.await.unwrap().unwrap();
    }
}

/// A slow queue sink cannot be bypassed by the Session close barrier.
struct HoldQueueSink {
    inner: Arc<dyn kernel::EventSink>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
#[async_trait]
impl kernel::EventSink for HoldQueueSink {
    /// Delays only queue snapshots so normal cancellation can settle independently.
    async fn emit(
        &self,
        event: protocol::AgentEvent,
    ) -> Result<(), kernel::SinkError> {
        if matches!(
            event.payload,
            protocol::AgentEventPayload::QueueChanged { .. }
        ) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.emit(event).await
    }
}

/// Closing waits for a blocked publication and includes the final empty snapshot.
#[tokio::test]
async fn close_waits_for_final_queue_delivery() {
    let test = TestKernel::new(|_| Route::Hang);
    let session = test.create("queue-close-delivery").await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let sink = Arc::new(HoldQueueSink {
        inner: test.sinks.sink(&session),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let kernel = test.kernel.clone();
    let id = session.clone();
    let run = tokio::spawn(async move {
        kernel
            .run_traced(
                RunRequest {
                    session_id: id,
                    input: "hold".into(),
                },
                sink,
                TraceId::try_from("trace-close").unwrap(),
                None,
            )
            .await
    });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let (prompt, rx) = test.client_prompt(&session, "q", None);
    test.kernel.prompt(prompt).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let kernel = test.kernel.clone();
    let id = session.clone();
    let close = tokio::spawn(async move { kernel.close_session(&id).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!close.is_finished());
    // Subsequent publications use the installed Session sink after the Run lease ends.
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    run.await.unwrap().unwrap();
    assert!(matches!(
        rx.await.unwrap(),
        Err(KernelError::PromptCancelled)
    ));
    let final_queue = test
        .sinks
        .payloads(&session)
        .into_iter()
        .filter_map(|event| match event {
            protocol::AgentEventPayload::QueueChanged { pending } => {
                Some(pending)
            }
            _ => None,
        })
        .next_back()
        .unwrap();
    assert!(
        final_queue.follow_up.is_empty() && final_queue.steering.is_empty()
    );
}

/// A transport failure before queued insertion must release the claim.
struct FailClaimSink {
    inner: Arc<dyn kernel::EventSink>,
    target: std::sync::Mutex<Option<protocol::MessageId>>,
}
#[async_trait]
impl kernel::EventSink for FailClaimSink {
    /// Rejects only the selected queued MessageStart, leaving settlement events deliverable.
    async fn emit(
        &self,
        event: protocol::AgentEvent,
    ) -> Result<(), kernel::SinkError> {
        if let protocol::AgentEventPayload::MessageStart { message_id } =
            &event.payload
            && self.target.lock().expect("target").as_ref() == Some(message_id)
        {
            return Err(kernel::SinkError::Consumer(
                "injected queued MessageStart failure".into(),
            ));
        }
        self.inner.emit(event).await
    }
}

/// Failure to deliver MessageStart leaves the claimed message pending and absent from history.
#[tokio::test]
async fn failed_message_start_returns_the_claim() {
    let release = Arc::new(Notify::new());
    let wait = Arc::clone(&release);
    let test = TestKernel::new(move |request| hold_route(&wait, request));
    let session = test.create("queue-sink-failure").await;
    let sink = Arc::new(FailClaimSink {
        inner: test.sinks.sink(&session),
        target: std::sync::Mutex::new(None),
    });
    let kernel = test.kernel.clone();
    let id = session.clone();
    let sink_run = Arc::clone(&sink);
    let run = tokio::spawn(async move {
        kernel
            .run_traced(
                RunRequest {
                    session_id: id,
                    input: "hold".into(),
                },
                sink_run,
                TraceId::try_from("trace-sink").expect("trace"),
                None,
            )
            .await
    });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let item = test
        .kernel
        .queue_message(&session, QueueKind::FollowUp, "q")
        .await
        .expect("queue");
    *sink.target.lock().expect("target") =
        Some(item.message.identity.message_id);
    release.notify_one();
    run.await.expect("join").expect_err("sink must fail");
    assert_eq!(test.user_texts(&session), ["hold"]);
    assert_eq!(
        test.kernel
            .pending_messages(&session)
            .expect("pending")
            .follow_up[0]
            .queue_id,
        item.queue_id
    );
}

/// Exercises reentrant input from an operation that owns the gate but has no open agent queue.
#[derive(Clone)]
struct SendFromBash;
#[async_trait]
impl ExtensionHandler<extension::UserBashPoint> for SendFromBash {
    /// Defers model input from inside the shell hook and then lets the shell finish.
    async fn handle(
        &self,
        _event: &protocol::UserBashEvent,
        context: &ExtensionContext,
    ) -> Result<Option<protocol::UserBashResult>, ExtensionError> {
        context
            .send_user_message(protocol::ExtensionUserMessage {
                text: "from extension".into(),
                delivery: QueueKind::FollowUp,
                parent_entry_id: None,
            })
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        Ok(None)
    }
}

/// A non-agent hook cannot await the operation gate it already owns.
#[tokio::test]
async fn extension_input_from_shell_hook_does_not_deadlock_or_strand() {
    let test = TestKernel::with_extensions(
        |_| Route::answer("answer"),
        super::insertion::hooks(|registrar| {
            registrar.on::<extension::UserBashPoint, _>(SendFromBash)
        }),
    );
    let session = test.create("queue-extension-bash").await;
    tokio::time::timeout(
        Duration::from_secs(5),
        test.start_run(&session, "!echo ready"),
    )
    .await
    .expect("no deadlock")
    .expect("join")
    .expect("shell");
    test.wait_until(|| {
        test.user_texts(&session).contains(&"from extension".into())
    })
    .await;
    assert_eq!(
        test.user_texts(&session)
            .iter()
            .filter(|text| *text == "from extension")
            .count(),
        1
    );
    assert!(
        test.kernel
            .pending_messages(&session)
            .expect("pending")
            .follow_up
            .is_empty()
    );
}

/// Pending client inputs receive one cancellation each from remove, clear, and repeated stop.
#[tokio::test]
async fn remove_clear_and_repeated_stop_settle_each_waiter_once() {
    let test = TestKernel::new(|_| Route::Hang);
    let session = test.create("queue-remove-clear").await;
    let run = test.start_run(&session, "hold");
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let mut receivers = Vec::new();
    for input in ["a", "b", "c"] {
        let (request, receiver) = test.client_prompt(&session, input, None);
        test.kernel.prompt(request).await.expect("queued");
        receivers.push(receiver);
    }
    let id = test
        .kernel
        .pending_messages(&session)
        .expect("pending")
        .follow_up[0]
        .queue_id
        .clone();
    test.kernel
        .remove_pending_message(&session, &id)
        .expect("remove");
    test.kernel
        .remove_pending_message(&session, &id)
        .expect("idempotent remove");
    test.kernel.clear_pending_messages(&session).expect("clear");
    test.kernel.cancel_session(&session).expect("stop");
    test.kernel.cancel_session(&session).expect("repeated stop");
    for receiver in receivers {
        assert!(matches!(
            receiver.await.expect("notice"),
            Err(KernelError::PromptCancelled)
        ));
    }
    run.await.expect("join").expect("cancelled run");
    assert_eq!(test.user_texts(&session), ["hold"]);
    assert!(
        test.kernel
            .pending_messages(&session)
            .expect("pending")
            .follow_up
            .is_empty()
    );
}

/// Successful direct commands must run before later admitted ordinary input.
#[tokio::test]
async fn successful_direct_command_precedes_later_input() {
    for command in ["!echo command", "/session"] {
        let release = Arc::new(Notify::new());
        let wait = Arc::clone(&release);
        let test = TestKernel::new(move |request| hold_route(&wait, request));
        let session = test.create("queue-command-success").await;
        let run = test.start_run(&session, "hold");
        test.wait_until(|| !test.model.last_texts().is_empty())
            .await;
        let (first, first_rx) = test.client_prompt(&session, command, None);
        let (second, second_rx) = test.client_prompt(&session, "after", None);
        let kernel = test.kernel.clone();
        let second = tokio::spawn(async move { kernel.prompt(second).await });
        let kernel = test.kernel.clone();
        let first = tokio::spawn(async move { kernel.prompt(first).await });
        release.notify_one();
        run.await.expect("join").expect("initial run");
        first.await.expect("join").expect("command");
        second.await.expect("join").expect("following run");
        let first = first_rx.await.expect("notice").expect("inserted");
        let second = second_rx.await.expect("notice").expect("inserted");
        let messages =
            test.kernel.session_transcript(&session).expect("history");
        assert!(
            messages.iter().position(|m| m.identity.message_id == first)
                < messages
                    .iter()
                    .position(|m| m.identity.message_id == second)
        );
    }
}

/// Blocks settlement after the final queue-empty check while the Run is still active.
#[derive(Clone)]
struct HoldEnd {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
#[async_trait]
impl ExtensionHandler<extension::AgentEndPoint> for HoldEnd {
    /// Holds only the initial Run's end hook, leaving the fallback Run free to complete.
    async fn handle(
        &self,
        event: &protocol::AgentEndEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        if event
            .messages
            .iter()
            .any(|m| super::fixture::text(&m.content) == "first")
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// A prompt arriving after the final queue check starts its own Run after settlement.
#[tokio::test]
async fn prompt_after_final_queue_check_cannot_be_stranded() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let hook = HoldEnd {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let test = TestKernel::with_extensions(
        |_| Route::answer("answer"),
        super::insertion::hooks(move |r| {
            r.on::<extension::AgentEndPoint, _>(hook.clone())
        }),
    );
    let session = test.create("queue-final-check").await;
    let first = test.start_run(&session, "first");
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("end hook");
    assert!(matches!(
        test.kernel
            .queue_message(&session, QueueKind::FollowUp, "reject")
            .await,
        Err(KernelError::SessionNotRunning(_))
    ));
    let (request, rx) = test.client_prompt(&session, "late", None);
    let kernel = test.kernel.clone();
    let late = tokio::spawn(async move { kernel.prompt(request).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!late.is_finished());
    release.notify_one();
    first.await.expect("join").expect("first");
    assert!(matches!(
        late.await.expect("join").expect("late"),
        kernel::PromptOutcome::Ran(_)
    ));
    rx.await.expect("notice").expect("inserted");
    assert_eq!(test.user_texts(&session), ["first", "late"]);
}

/// Idle admission is normal control flow and must not produce an error-level queue failure.
#[tokio::test]
async fn idle_prompt_does_not_log_an_enqueue_error() {
    let logs = super::support::CapturedLogs::default();
    let test = TestKernel::new(|_| Route::answer("done"));
    let session = test.create("idle-admission-log-review").await;
    let (request, rx) = test.client_prompt(&session, "hello", None);
    test.kernel.prompt(request).await.expect("idle prompt");
    rx.await.expect("notice").expect("inserted");
    assert!(
        !logs
            .content()
            .lines()
            .any(|line| line.contains(session.as_str())
                && line.contains("ERROR")),
        "normal idle prompt emitted an error"
    );
}
