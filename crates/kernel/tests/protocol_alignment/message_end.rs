use std::sync::Arc;

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionModule,
    ExtensionRegistrar, MessageEndPoint, StaticExtensionFactory,
};
use kernel::{AnswerOutcome, SessionEventSinks, WhenBusy};
use protocol::{
    AgentEventPayload, AgentMessage, ContentBlock, ExtensionDescriptor,
    ExtensionId, MessageContent, MessageEndEvent, MessageId, ModelStreamEvent,
    QueueKind, RunRequest, SessionId, StaticExtensionRegistration, StopReason,
};
use tokio::sync::Notify;

use super::fixture::{Route, TestKernel, finished, request, text};

/// What the hook changes on the message it targets.
#[derive(Clone, Copy, Debug)]
enum Rewrite {
    Identity,
    Role,
    Text,
}

/// Rewrites one message from `message_end` so its validation can be observed.
#[derive(Clone)]
struct RewriteMessage {
    target: &'static str,
    rewrite: Rewrite,
}

impl RewriteMessage {
    /// Wraps this hook in the static extension factory the Kernel fixture accepts.
    fn factory(self) -> Arc<StaticExtensionFactory> {
        Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(self.clone()) as Arc<dyn ExtensionModule>)
            })],
        ))
    }

    /// Builds a Kernel whose "hold" Turn waits on `gate`, leaving time to queue input.
    fn holding_kernel(self, gate: Arc<Notify>) -> TestKernel {
        TestKernel::with_extensions(
            move |request| {
                let last = request.messages.last().expect("request message");
                if text(&last.content) == "hold" {
                    Route::Wait(
                        Arc::clone(&gate),
                        vec![
                            ModelStreamEvent::TextDelta("held".into()),
                            finished(StopReason::EndTurn),
                        ],
                    )
                } else {
                    Route::answer("answer")
                }
            },
            self.factory(),
        )
    }
}

#[async_trait]
impl ExtensionHandler<MessageEndPoint> for RewriteMessage {
    /// Replaces only the targeted message and leaves the rest of the Run untouched.
    async fn handle(
        &self,
        event: &MessageEndEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<AgentMessage>, ExtensionError> {
        if text(&event.message.content) != self.target {
            return Ok(None);
        }
        let mut message = event.message.clone();
        match self.rewrite {
            Rewrite::Identity => {
                message.identity.message_id =
                    MessageId::try_from("message-replaced").expect("id");
            }
            Rewrite::Role => {
                message.content = MessageContent::System {
                    blocks: vec![ContentBlock::Text {
                        text: "forged".into(),
                    }],
                };
            }
            Rewrite::Text => {
                message.content = MessageContent::User {
                    blocks: vec![ContentBlock::Text {
                        text: "rewritten".into(),
                    }],
                };
            }
        }
        Ok(Some(message))
    }
}

impl ExtensionModule for RewriteMessage {
    /// Identifies the rewriting test extension.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("message-rewriter").expect("id"),
            name: "Message rewriter".into(),
            version: "1".into(),
        }
    }

    /// Registers the message_end replacement.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<MessageEndPoint, _>(self.clone())
    }
}

/// Reports whether the faulty replacement surfaced as a `message_end` diagnostic.
fn reported_message_end(test: &TestKernel, session: &SessionId) -> bool {
    test.sinks.saw(session, |event| {
        matches!(
            event,
            AgentEventPayload::ExtensionHandlerFailed { point, .. }
                if point == "message_end"
        )
    })
}

/// A prompt keeps its identity and role, and the Run still answers it.
#[tokio::test]
async fn message_end_discards_user_identity_or_role_change() {
    for rewrite in [Rewrite::Identity, Rewrite::Role] {
        let test = TestKernel::with_extensions(
            |_| Route::answer("answer"),
            RewriteMessage {
                target: "hello",
                rewrite,
            }
            .factory(),
        );
        let session = test.create("message-end-prompt").await;
        test.kernel
            .run(
                RunRequest {
                    session_id: session.clone(),
                    input: "hello".into(),
                },
                test.sinks.sink(&session),
            )
            .await
            .unwrap_or_else(|error| panic!("{rewrite:?} failed: {error}"));
        assert_eq!(test.user_texts(&session), ["hello"], "{rewrite:?}");
        assert_eq!(test.model.last_texts(), ["hello"], "{rewrite:?}");
        assert!(reported_message_end(&test, &session), "{rewrite:?}");
    }
}

/// An Assistant replacement that changes its role cannot break settlement.
#[tokio::test]
async fn message_end_discards_assistant_identity_or_role_change() {
    for rewrite in [Rewrite::Identity, Rewrite::Role] {
        let test = TestKernel::with_extensions(
            |_| Route::answer("answer"),
            RewriteMessage {
                target: "answer",
                rewrite,
            }
            .factory(),
        );
        let session = test.create("message-end-assistant").await;
        let result = test
            .kernel
            .run(
                RunRequest {
                    session_id: session.clone(),
                    input: "hello".into(),
                },
                test.sinks.sink(&session),
            )
            .await
            .unwrap_or_else(|error| panic!("{rewrite:?} failed: {error}"));
        let assistant = result.messages.last().expect("assistant");
        assert!(
            matches!(assistant.content, MessageContent::Assistant { .. }),
            "{rewrite:?}"
        );
        assert_ne!(
            assistant.identity.message_id.as_str(),
            "message-replaced",
            "{rewrite:?}"
        );
        assert_eq!(text(&assistant.content), "answer", "{rewrite:?}");
        assert!(reported_message_end(&test, &session), "{rewrite:?}");
    }
}

/// A faulty hook consumes queued input normally, so later Runs are unaffected.
#[tokio::test]
async fn message_end_rewrite_leaves_queue_usable() {
    for rewrite in [Rewrite::Identity, Rewrite::Role] {
        let release = Arc::new(Notify::new());
        let test = RewriteMessage {
            target: "queued",
            rewrite,
        }
        .holding_kernel(Arc::clone(&release));
        let session = test.create("message-end-queue").await;
        let kernel = test.kernel.clone();
        let sink = test.sinks.sink(&session);
        let run_session = session.clone();
        let run = tokio::spawn(async move {
            kernel
                .run(
                    RunRequest {
                        session_id: run_session,
                        input: "hold".into(),
                    },
                    sink,
                )
                .await
        });
        test.wait_until(|| !test.model.last_texts().is_empty())
            .await;
        test.kernel
            .queue_message(&session, QueueKind::FollowUp, "queued")
            .await
            .expect("queue");
        release.notify_one();
        run.await
            .expect("join")
            .unwrap_or_else(|error| panic!("{rewrite:?} failed: {error}"));
        assert!(
            test.kernel
                .pending_messages(&session)
                .expect("pending")
                .follow_up
                .is_empty(),
            "{rewrite:?} left the message queued"
        );

        test.kernel
            .run(
                RunRequest {
                    session_id: session.clone(),
                    input: "next".into(),
                },
                test.sinks.sink(&session),
            )
            .await
            .unwrap_or_else(|error| panic!("{rewrite:?} next: {error}"));

        assert_eq!(
            test.user_texts(&session),
            ["hold", "queued", "next"],
            "{rewrite:?}"
        );
        assert_eq!(
            test.model.last_texts(),
            ["hold", "queued", "next"],
            "{rewrite:?}"
        );
    }
}

/// A queued submitter gets the real answer instead of a misleading cancellation.
#[tokio::test]
async fn message_end_rewrite_still_answers_queued_submitter() {
    let release = Arc::new(Notify::new());
    let test = RewriteMessage {
        target: "queued",
        rewrite: Rewrite::Identity,
    }
    .holding_kernel(Arc::clone(&release));
    let session = test.create("message-end-submit").await;
    let kernel = test.kernel.clone();
    let hold = request(&session, "hold", WhenBusy::FollowUp);
    let first = tokio::spawn(async move { kernel.submit(hold).await });
    test.wait_until(|| !test.model.last_texts().is_empty())
        .await;
    let kernel = test.kernel.clone();
    let queued = request(&session, "queued", WhenBusy::FollowUp);
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

    let answer = second.await.expect("join").expect("answer");

    assert!(
        matches!(answer.outcome, AnswerOutcome::Completed),
        "unexpected outcome: {:?}",
        answer.outcome
    );
    assert_eq!(answer.text, "answer");
    first.await.expect("join").expect("first answer");
}

/// Rewriting what a user message says stays allowed; the model sees the new text.
#[tokio::test]
async fn message_end_may_rewrite_user_text() {
    let test = TestKernel::with_extensions(
        |_| Route::answer("done"),
        RewriteMessage {
            target: "hello",
            rewrite: Rewrite::Text,
        }
        .factory(),
    );
    let session = test.create("message-end-text").await;
    test.kernel
        .run(
            RunRequest {
                session_id: session.clone(),
                input: "hello".into(),
            },
            test.sinks.sink(&session),
        )
        .await
        .expect("run");
    assert_eq!(test.user_texts(&session), ["rewritten"]);
    assert_eq!(test.model.last_texts(), ["rewritten"]);
    assert!(!reported_message_end(&test, &session));
}
