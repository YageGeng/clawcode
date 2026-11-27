//! `request_user_input_async`: non-blocking questions, answers, and skips.

use std::sync::Arc;

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, MessageStartPoint,
};
use kernel::{
    InsertionNotice, InsertionReceiver, KernelError, PromptOutcome,
    PromptRequest, SessionEventSinks,
};
use protocol::{
    AsyncQuestion, CompactionPolicy, MessageContent, MessageStartEvent,
    ModelRequest, ModelStreamEvent, QuestionKey, QuestionReply,
    QuestionReplyItem, RunInput, RunRequest, RunResult, SessionId, SessionMode,
    StopReason, ToolCall, ToolCallId, ToolResultDetails,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::fixture::{
    Route, TestKernel, calls, text, tool_failed, tool_result,
};

/// Tool call id every scripted ask uses.
const CALL: &str = "call-q";
/// Tool call id of the extra call that keeps a held Run going.
const CONTINUE_CALL: &str = "call-todo";

/// What the scripted model does in the request after the question's result.
#[derive(Clone)]
enum AfterAsk {
    /// Answers at once, ending the Run.
    Answer,
    /// Holds a final answer until released, so the Run ends after it.
    Hold(Arc<Notify>),
    /// Holds one more tool call until released, so the Run continues.
    HoldAndContinue(Arc<Notify>),
}

/// A scripted model: `ask` calls the question tool with `arguments`.
#[derive(Clone)]
struct Script {
    arguments: serde_json::Value,
    after: AfterAsk,
}

impl Script {
    /// Asks the default two-option, multi-select question.
    fn which_db(after: AfterAsk) -> Self {
        Self {
            arguments: serde_json::json!({
                "questions": [{ "title": "Which db?", "options": ["sqlite", "postgres"], "multi_select": true }]
            }),
            after,
        }
    }

    /// Answers one provider request.
    fn route(&self, request: &ModelRequest) -> Route {
        if request.messages.first().is_some_and(|message| {
            text(&message.content)
                .starts_with("You are a context summarization assistant")
        }) {
            return Route::answer("summary");
        }
        // Notes may trail a tool result, so the latest model step decides.
        let latest_result = request.messages.iter().rev().find_map(|message| {
            match &message.content {
                MessageContent::ToolResult { tool_call_id, .. } => {
                    Some(Some(tool_call_id.as_str()))
                }
                MessageContent::Assistant { .. } => Some(None),
                _ => None,
            }
        });
        let finished = |stop| super::fixture::finished(stop);
        match (latest_result.flatten(), &self.after) {
            (Some(CALL), AfterAsk::Hold(release)) => Route::Wait(
                Arc::clone(release),
                vec![
                    ModelStreamEvent::TextDelta("working".into()),
                    finished(StopReason::EndTurn),
                ],
            ),
            (Some(CALL), AfterAsk::HoldAndContinue(release)) => Route::Wait(
                Arc::clone(release),
                vec![
                    ModelStreamEvent::ToolCall(
                        ToolCall::builder()
                            .tool_call_id(
                                ToolCallId::try_from(CONTINUE_CALL)
                                    .expect("id"),
                            )
                            .name("todo_write".into())
                            .arguments(serde_json::json!({ "todos": [] }))
                            .build(),
                    ),
                    finished(StopReason::ToolUse),
                ],
            ),
            (Some(_), _) => Route::answer("continuing"),
            (None, _) => match request
                .messages
                .last()
                .map(|message| text(&message.content))
                .as_deref()
            {
                Some("ask") => calls(&[(
                    CALL,
                    "request_user_input_async",
                    self.arguments.clone(),
                )]),
                _ => Route::answer("continuing"),
            },
        }
    }

    /// Builds a Kernel driven by this script.
    fn kernel(self) -> TestKernel {
        TestKernel::new(move |request| self.route(request))
    }

    /// Builds a Kernel whose question replies block in `message_start`.
    fn holding(self, hold: HoldReply) -> TestKernel {
        TestKernel::with_extensions(
            move |request| self.route(request),
            super::insertion::hooks(move |registrar| {
                registrar.on::<MessageStartPoint, _>(hold.clone())
            }),
        )
    }
}

/// One Session that asked a question, with the asking call's request id.
struct Asked {
    test: TestKernel,
    session: SessionId,
    request_id: String,
}

impl Asked {
    /// Runs `ask` to completion on a fresh Session.
    async fn idle(test: TestKernel, name: &str) -> Self {
        let session = test.create(name).await;
        test.run_text(&session, "ask").await.expect("ask Run");
        Self::of(test, session)
    }

    /// Starts `ask` and returns once the Run holds after the question's result.
    async fn held(
        test: TestKernel,
        name: &str,
    ) -> (Self, JoinHandle<Result<RunResult, KernelError>>) {
        let session = test.create(name).await;
        let run = test.start(&session, "ask");
        test.wait_until(async || test.model.request_count() == 2)
            .await;
        (Self::of(test, session), run)
    }

    /// Reads the request id of the Session's latest ask.
    fn of(test: TestKernel, session: SessionId) -> Self {
        let request_id = match tool_result(&test, &session, CALL).1 {
            Some(ToolResultDetails::UserQuestions { request_id, .. }) => {
                Some(request_id)
            }
            _ => None,
        }
        .expect("question details");
        Self {
            test,
            session,
            request_id,
        }
    }

    /// The key of question `index` of this ask.
    fn key(&self, index: usize) -> QuestionKey {
        QuestionKey {
            request_id: self.request_id.clone(),
            index,
        }
    }

    /// Allocates a real ingress ticket for one client input, like an ACP prompt.
    fn request(&self, input: RunInput) -> (PromptRequest, InsertionReceiver) {
        let (notice, receiver) = InsertionNotice::new();
        let request = PromptRequest::builder()
            .run(RunRequest {
                session_id: self.session.clone(),
                input,
            })
            .sink(self.test.sinks.sink(&self.session))
            .ticket(
                self.test
                    .kernel
                    .admission_ticket(&self.session)
                    .expect("ticket"),
            )
            .insertion(notice)
            .build();
        (request, receiver)
    }

    /// Builds the client prompt answering question `index` with `answer`.
    fn prompt(
        &self,
        index: usize,
        answer: &str,
    ) -> (PromptRequest, InsertionReceiver) {
        self.request(RunInput::QuestionReply(QuestionReply {
            items: vec![
                QuestionReplyItem::builder()
                    .request_id(self.request_id.clone())
                    .index(index)
                    .answer(Some(answer.into()))
                    .build(),
            ],
        }))
    }

    /// Sends one plain client prompt and returns its admission outcome.
    async fn say(&self, text: &str) -> Result<PromptOutcome, KernelError> {
        let (request, _receiver) = self.request(RunInput::Text(text.into()));
        self.test.kernel.prompt(request).await
    }

    /// Submits one answer and returns its admission outcome.
    async fn answer(
        &self,
        index: usize,
        answer: &str,
    ) -> Result<PromptOutcome, KernelError> {
        let (request, _receiver) = self.prompt(index, answer);
        self.test.kernel.prompt(request).await
    }

    /// Skips the questions at `indexes`.
    async fn dismiss(&self, indexes: &[usize]) -> Result<usize, KernelError> {
        self.test
            .kernel
            .dismiss_questions(
                &self.session,
                indexes.iter().map(|index| self.key(*index)).collect(),
            )
            .await
    }

    /// The reply recording a skip of question `index`, titled as the kernel
    /// copies it from the default question.
    fn skip(&self, index: usize) -> QuestionReply {
        QuestionReply {
            items: vec![
                QuestionReplyItem::builder()
                    .request_id(self.request_id.clone())
                    .index(index)
                    .question("Which db?".into())
                    .build(),
            ],
        }
    }

    /// Every question reply on the Session's branch, in order.
    fn replies(&self) -> Vec<QuestionReply> {
        self.test
            .kernel
            .session_transcript(&self.session)
            .expect("transcript")
            .iter()
            .filter_map(|message| {
                QuestionReply::try_from(&message.content).ok()
            })
            .collect()
    }

    /// Every provider request the scripted model received.
    fn requests(&self) -> Vec<ModelRequest> {
        self.test.model.requests.lock().expect("requests").clone()
    }
}

/// Reports whether `result` is the closed-question refusal for `index`.
fn is_closed(
    result: &Result<PromptOutcome, KernelError>,
    index: usize,
) -> bool {
    matches!(
        result,
        Err(KernelError::QuestionClosed { index: closed, .. }) if *closed == index
    )
}

/// Blocks the message start of every question reply until released.
#[derive(Clone)]
struct HoldReply {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ExtensionHandler<MessageStartPoint> for HoldReply {
    /// Holds a reply after it left the queue or became a Run's first input.
    async fn handle(
        &self,
        event: &MessageStartEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        if QuestionReply::try_from(&event.message.content).is_ok() {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

impl HoldReply {
    /// A hook that has not been entered or released yet.
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
}

/// The call returns at once, the Run continues, and the questions persist.
#[tokio::test]
async fn ask_returns_immediately_and_persists_questions() {
    let asked =
        Asked::idle(Script::which_db(AfterAsk::Answer).kernel(), "ask-returns")
            .await;

    assert_eq!(asked.test.model.request_count(), 2);
    let (text, details) = tool_result(&asked.test, &asked.session, CALL);
    assert_eq!(text, r#"{"accepted":true}"#);
    assert!(!tool_failed(&asked.test, &asked.session, CALL));
    assert!(!asked.request_id.is_empty());
    assert_eq!(
        details,
        Some(ToolResultDetails::UserQuestions {
            request_id: asked.request_id.clone(),
            questions: vec![AsyncQuestion {
                title: "Which db?".into(),
                options: Some(vec!["sqlite".into(), "postgres".into()]),
                multi_select: true,
            }],
        })
    );
}

/// Every argument outside the limits is refused to the model without details.
#[tokio::test]
async fn invalid_questions_are_refused_to_the_model() {
    let question = |title: String| serde_json::json!({ "title": title });
    let with_options = |options: Vec<String>| serde_json::json!({ "questions": [{ "title": "T", "options": options }] });
    let cases = vec![
        serde_json::json!({ "questions": [] }),
        serde_json::json!({ "questions": vec![question("T".into()); 9] }),
        serde_json::json!({ "questions": [question("  ".into())] }),
        serde_json::json!({ "questions": [question("x".repeat(2049))] }),
        with_options(Vec::new()),
        with_options(vec!["Other".into()]),
        with_options(vec![" other ".into()]),
        with_options(vec!["x".repeat(513)]),
        with_options((0..9).map(|n| n.to_string()).collect()),
        serde_json::json!({ "questions": [question("T".into())], "extra": 1 }),
        serde_json::json!({ "questions": [{ "title": "T", "extra": 1 }] }),
    ];
    for (position, arguments) in cases.into_iter().enumerate() {
        let test = Script {
            arguments: arguments.clone(),
            after: AfterAsk::Answer,
        }
        .kernel();
        let session = test.create(&format!("ask-invalid-{position}")).await;
        test.run_text(&session, "ask").await.expect("ask Run");
        assert!(tool_failed(&test, &session, CALL), "{arguments}");
        let (text, details) = tool_result(&test, &session, CALL);
        assert!(
            text.contains("invalid arguments for request_user_input_async"),
            "{arguments}: {text}"
        );
        assert_eq!(details, None, "{arguments}");
    }
}

/// Plan mode trusts the kernel-owned tool.
#[tokio::test]
async fn plan_mode_runs_the_tool() {
    let test = Script::which_db(AfterAsk::Answer).kernel();
    let session = test.create("ask-plan").await;
    test.kernel
        .set_session_mode(&session, SessionMode::Plan)
        .await
        .expect("plan mode");
    test.run_text(&session, "ask").await.expect("ask Run");
    assert!(!tool_failed(&test, &session, CALL));
}

/// An idle answer starts a Run whose model reads the Codex reply envelope.
#[tokio::test]
async fn idle_answer_starts_a_run_with_the_reply_envelope() {
    let asked =
        Asked::idle(Script::which_db(AfterAsk::Answer).kernel(), "answer-idle")
            .await;

    let (request, receiver) = asked.prompt(0, "postgres");
    let outcome = asked.test.kernel.prompt(request).await.expect("answer");
    assert!(matches!(outcome, PromptOutcome::Ran(_)));
    assert!(matches!(receiver.await, Ok(Ok(_))));
    let expected = QuestionReply {
        items: vec![
            QuestionReplyItem::builder()
                .request_id(asked.request_id.clone())
                .index(0)
                .question("Which db?".into())
                .answer(Some("postgres".into()))
                .build(),
        ],
    }
    .to_string();
    let requests = asked.requests();
    let last = requests
        .last()
        .and_then(|request| request.messages.last())
        .expect("message");
    QuestionReply::try_from(&last.content).expect("question reply");
    assert_eq!(text(&last.content), expected);
}

/// A busy answer steers into the running Run before its next model call.
#[tokio::test]
async fn busy_answer_is_steered_before_the_next_model_call() {
    let release = Arc::new(Notify::new());
    let script = Script::which_db(AfterAsk::Hold(Arc::clone(&release)));
    let (asked, run) = Asked::held(script.kernel(), "answer-busy").await;

    let (request, receiver) = asked.prompt(0, "postgres");
    assert!(matches!(
        asked.test.kernel.prompt(request).await.expect("answer"),
        PromptOutcome::Queued(_)
    ));
    let pending = asked
        .test
        .kernel
        .pending_messages(&asked.session)
        .expect("pending");
    assert_eq!((pending.steering.len(), pending.follow_up.len()), (1, 0));
    release.notify_one();
    run.await.expect("join").expect("Run");
    assert!(matches!(receiver.await, Ok(Ok(_))));
    let requests = asked.requests();
    assert_eq!(requests.len(), 3);
    let last = requests[2].messages.last().expect("message");
    QuestionReply::try_from(&last.content).expect("question reply");
    assert!(text(&last.content).contains("postgres"));
}

/// Answers to answered, unknown, or out-of-range questions are refused.
#[tokio::test]
async fn closed_questions_reject_answers() {
    let asked = Asked::idle(
        Script::which_db(AfterAsk::Answer).kernel(),
        "answer-closed",
    )
    .await;
    asked.answer(0, "a").await.expect("answer");
    let (request, receiver) = asked.prompt(0, "b");
    assert!(is_closed(&asked.test.kernel.prompt(request).await, 0));
    assert!(matches!(
        receiver.await,
        Ok(Err(KernelError::QuestionClosed { .. }))
    ));
    assert!(is_closed(&asked.answer(1, "a").await, 1));
    let unknown = Asked {
        request_id: "unknown".into(),
        ..asked
    };
    assert!(is_closed(&unknown.answer(0, "a").await, 0));

    let release = Arc::new(Notify::new());
    let script = Script::which_db(AfterAsk::Hold(Arc::clone(&release)));
    let (busy, run) = Asked::held(script.kernel(), "answer-closed-busy").await;
    assert!(matches!(
        busy.answer(0, "a").await,
        Ok(PromptOutcome::Queued(_))
    ));
    assert!(is_closed(&busy.answer(0, "b").await, 0));
    release.notify_one();
    run.await.expect("join").expect("Run");
}

/// Answers not yet written, as a first input or a claimed item, close the question.
#[tokio::test]
async fn in_flight_answers_close_the_question() {
    let hold = HoldReply::new();
    let asked = Asked::idle(
        Script::which_db(AfterAsk::Answer).holding(hold.clone()),
        "answer-in-flight",
    )
    .await;
    let (request, _receiver) = asked.prompt(0, "first");
    let kernel = asked.test.kernel.clone();
    let first = tokio::spawn(async move { kernel.prompt(request).await });
    hold.entered.notified().await;
    assert!(is_closed(&asked.answer(0, "second").await, 0));
    hold.release.notify_one();
    assert!(matches!(
        first.await.expect("join"),
        Ok(PromptOutcome::Ran(_))
    ));

    let release = Arc::new(Notify::new());
    let hold = HoldReply::new();
    let script = Script::which_db(AfterAsk::Hold(Arc::clone(&release)));
    let (busy, run) =
        Asked::held(script.holding(hold.clone()), "answer-claimed").await;
    assert!(matches!(
        busy.answer(0, "first").await,
        Ok(PromptOutcome::Queued(_))
    ));
    release.notify_one();
    hold.entered.notified().await;
    let pending = busy
        .test
        .kernel
        .pending_messages(&busy.session)
        .expect("pending");
    assert!(pending.steering.is_empty());
    assert_eq!(
        serde_json::to_value(&pending).unwrap()["reservedQuestions"],
        serde_json::json!([busy.key(0)])
    );
    assert!(is_closed(&busy.answer(0, "second").await, 0));
    hold.release.notify_one();
    run.await.expect("join").expect("Run");
}

/// An unwritten first-input answer reserves its key live and releases it again if cancelled.
#[tokio::test]
async fn cancelled_first_answer_publishes_the_reopened_question() {
    let hold = HoldReply::new();
    let asked = Asked::idle(
        Script::which_db(AfterAsk::Answer).holding(hold.clone()),
        "answer-cancel-reservation",
    )
    .await;
    let (request, _receiver) = asked.prompt(0, "first");
    let kernel = asked.test.kernel.clone();
    let first = tokio::spawn(async move { kernel.prompt(request).await });
    hold.entered.notified().await;
    let key = asked.key(0);
    asked.test.wait_until(async || {
        asked.test.sinks.saw(&asked.session, |event| matches!(event,
            protocol::AgentEventPayload::QueueChanged { pending } if pending.reserved_questions.contains(&key)))
    }).await;
    let boundary = asked.test.sinks.events(&asked.session).len();
    asked
        .test
        .kernel
        .cancel_session(&asked.session)
        .expect("cancel");
    hold.release.notify_one();
    assert!(matches!(
        first.await.expect("join"),
        Err(KernelError::PromptCancelled)
    ));
    assert!(asked.replies().is_empty());
    asked.test.wait_until(async || {
        asked.test.sinks.events(&asked.session)[boundary..].iter().any(|event|
            matches!(&event.payload, protocol::AgentEventPayload::QueueChanged { pending } if pending.reserved_questions.is_empty()))
    }).await;
}

/// Stopping returns a queued answer, and the question opens again.
#[tokio::test]
async fn stopping_returns_a_queued_answer_and_reopens_the_question() {
    let release = Arc::new(Notify::new());
    let script = Script::which_db(AfterAsk::Hold(Arc::clone(&release)));
    let (asked, run) = Asked::held(script.kernel(), "answer-stop").await;
    let (request, receiver) = asked.prompt(0, "postgres");
    assert!(matches!(
        asked.test.kernel.prompt(request).await.expect("answer"),
        PromptOutcome::Queued(_)
    ));
    asked
        .test
        .kernel
        .cancel_session(&asked.session)
        .expect("cancel");
    assert!(matches!(
        receiver.await,
        Ok(Err(KernelError::PromptCancelled))
    ));
    let _ = run.await.expect("join");
    assert!(matches!(
        asked.answer(0, "postgres").await,
        Ok(PromptOutcome::Ran(_))
    ));
}

/// Leaving the branch that asked closes its questions.
#[tokio::test]
async fn switching_branch_closes_the_question() {
    let asked = Asked::idle(
        Script::which_db(AfterAsk::Answer).kernel(),
        "answer-branch",
    )
    .await;
    asked
        .test
        .kernel
        .navigate_session(&asked.session, None)
        .await
        .expect("navigate");
    assert!(is_closed(&asked.answer(0, "a").await, 0));
}

/// Calls that reuse one tool call id in different Turns stay independent.
#[tokio::test]
async fn reused_tool_call_ids_stay_independent() {
    let first = Asked::idle(
        Script::which_db(AfterAsk::Answer).kernel(),
        "answer-reused-id",
    )
    .await;
    first
        .test
        .run_text(&first.session, "ask")
        .await
        .expect("second ask");
    let first_id = first.request_id.clone();
    let mut asked = Asked::of(first.test, first.session);
    let second_id = asked.request_id.clone();
    assert_ne!(first_id, second_id);
    asked.request_id = first_id;
    asked.answer(0, "a").await.expect("answer");
    asked.request_id = second_id;
    asked.answer(0, "b").await.expect("answer");
}

/// Compaction keeps the asking result on the branch, so answers still land.
#[tokio::test]
async fn answer_after_compaction_is_accepted() {
    let script = Script::which_db(AfterAsk::Answer);
    let test = TestKernel::with_compaction(
        move |request: &ModelRequest| script.route(request),
        CompactionPolicy::builder()
            .enabled(false)
            .reserve_tokens(16_384)
            .keep_recent_tokens(1)
            .build(),
    );
    let asked = Asked::idle(test, "answer-compacted").await;
    asked
        .test
        .run_text(&asked.session, "more")
        .await
        .expect("second Run");
    asked
        .test
        .kernel
        .compact_session(&asked.session, asked.test.sinks.sink(&asked.session))
        .await
        .expect("compaction");
    assert!(matches!(
        asked.answer(0, "postgres").await,
        Ok(PromptOutcome::Ran(_))
    ));
}

/// A queued answer whose submitter left survives reopening the Session.
#[tokio::test]
async fn queued_answer_survives_reopen() {
    let release = Arc::new(Notify::new());
    let script = Script::which_db(AfterAsk::Hold(Arc::clone(&release)));
    let (asked, run) = Asked::held(script.kernel(), "answer-reopen").await;
    // A long queued answer still occupies just one identity on both sides of recovery.
    let (request, receiver) = asked.prompt(0, &"p".repeat(64 * 1024));
    drop(receiver);
    assert!(matches!(
        asked.test.kernel.prompt(request).await.expect("answer"),
        PromptOutcome::Queued(_)
    ));
    assert_eq!(
        asked
            .test
            .kernel
            .reserved_questions(&asked.session)
            .unwrap(),
        [asked.key(0)].into()
    );
    asked
        .test
        .kernel
        .close_session(&asked.session)
        .await
        .expect("close Session");
    let _ = run.await.expect("join");
    asked
        .test
        .kernel
        .resume_session(
            asked.session.clone(),
            asked.test.root.path().to_path_buf(),
        )
        .await
        .expect("resume Session");
    assert_eq!(
        asked
            .test
            .kernel
            .reserved_questions(&asked.session)
            .unwrap(),
        [asked.key(0)].into()
    );
    assert!(is_closed(&asked.answer(0, "again").await, 0));
    let before = asked.test.model.request_count();
    asked
        .test
        .run_text(&asked.session, "next")
        .await
        .expect("next Run");
    assert!(asked.requests()[before..].iter().any(|request| {
        request
            .messages
            .iter()
            .any(|message| QuestionReply::try_from(&message.content).is_ok())
    }));
}

/// An idle skip is written at once and starts no Run.
#[tokio::test]
async fn idle_skip_writes_without_starting_a_run() {
    let asked =
        Asked::idle(Script::which_db(AfterAsk::Answer).kernel(), "skip-idle")
            .await;
    let before = asked.test.model.request_count();

    assert_eq!(asked.dismiss(&[0]).await.expect("dismiss"), 1);
    assert_eq!(asked.test.model.request_count(), before);
    assert_eq!(asked.replies(), vec![asked.skip(0)]);

    asked
        .test
        .run_text(&asked.session, "next")
        .await
        .expect("next Run");
    let requests = asked.requests();
    let messages = &requests.last().expect("request").messages;
    let reply = messages
        .iter()
        .position(|message| QuestionReply::try_from(&message.content).is_ok());
    let next = messages
        .iter()
        .position(|message| text(&message.content) == "next");
    assert!(reply.is_some() && reply < next, "{reply:?} {next:?}");
}

/// A busy skip enters history before the Run's next model request, never the queue.
#[tokio::test]
async fn busy_skip_is_written_before_the_next_model_request() {
    let release = Arc::new(Notify::new());
    let script =
        Script::which_db(AfterAsk::HoldAndContinue(Arc::clone(&release)));
    let (asked, run) = Asked::held(script.kernel(), "skip-busy").await;

    assert_eq!(asked.dismiss(&[0]).await.expect("dismiss"), 1);
    let pending = asked
        .test
        .kernel
        .pending_messages(&asked.session)
        .expect("pending");
    assert!(pending.steering.is_empty() && pending.follow_up.is_empty());
    let expected = serde_json::json!([asked.key(0)]);
    assert_eq!(
        serde_json::to_value(&pending).unwrap()["reservedQuestions"],
        expected
    );
    asked.test.wait_until(async || {
        asked.test.sinks.saw(&asked.session, |event| matches!(event,
            protocol::AgentEventPayload::QueueChanged { pending }
                if serde_json::to_value(pending).unwrap()["reservedQuestions"] == expected))
    }).await;
    release.notify_one();
    run.await.expect("join").expect("Run");
    let requests = asked.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .messages
            .iter()
            .any(|message| QuestionReply::try_from(&message.content).is_ok())
    );
}

/// Dropping a Run before its write-back reopens a held skip for all clients.
#[tokio::test]
async fn dropped_run_releases_held_skip() {
    let release = Arc::new(Notify::new());
    let script = Script::which_db(AfterAsk::Hold(release));
    let (asked, run) = Asked::held(script.kernel(), "skip-drop").await;
    let key = asked.key(0);
    assert_eq!(asked.dismiss(&[0]).await.unwrap(), 1);
    asked
        .test
        .wait_until(async || {
            asked.test.sinks.saw(&asked.session, |event| {
                matches!(
                    event,
                    protocol::AgentEventPayload::QueueChanged { pending }
                        if pending.reserved_questions.contains(&key)
                )
            })
        })
        .await;
    let boundary = asked.test.sinks.events(&asked.session).len();
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    assert!(asked.replies().is_empty());
    assert!(
        asked
            .test
            .kernel
            .reserved_questions(&asked.session)
            .unwrap()
            .is_empty()
    );
    asked
        .test
        .wait_until(async || {
            asked.test.sinks.events(&asked.session)[boundary..]
                .iter()
                .any(|event| {
                    matches!(
                        &event.payload,
                        protocol::AgentEventPayload::QueueChanged { pending }
                            if pending.reserved_questions.is_empty()
                    )
                })
        })
        .await;
    assert_eq!(asked.dismiss(&[0]).await.unwrap(), 1);
    assert_eq!(asked.replies(), vec![asked.skip(0)]);
}

/// A skip during the final Turn is written back at Run close without another model call.
#[tokio::test]
async fn skip_during_the_final_turn_costs_no_model_call() {
    let release = Arc::new(Notify::new());
    let script = Script::which_db(AfterAsk::Hold(Arc::clone(&release)));
    let (asked, run) = Asked::held(script.kernel(), "skip-final").await;

    assert_eq!(asked.dismiss(&[0]).await.expect("dismiss"), 1);
    release.notify_one();
    run.await.expect("join").expect("Run");
    assert_eq!(asked.test.model.request_count(), 2);
    assert_eq!(asked.replies(), vec![asked.skip(0)]);
}

/// Skipping settled or repeated questions writes nothing more.
#[tokio::test]
async fn dismiss_is_idempotent() {
    let asked =
        Asked::idle(Script::which_db(AfterAsk::Answer).kernel(), "skip-twice")
            .await;
    assert_eq!(asked.dismiss(&[0]).await.expect("first"), 1);
    assert_eq!(asked.dismiss(&[0]).await.expect("second"), 0);
    assert_eq!(asked.replies().len(), 1);

    let repeated = Asked::idle(
        Script::which_db(AfterAsk::Answer).kernel(),
        "skip-repeated",
    )
    .await;
    assert_eq!(repeated.dismiss(&[0, 0]).await.expect("dismiss"), 1);
    assert_eq!(repeated.replies(), vec![repeated.skip(0)]);
}

/// A concurrent answer and skip of one question settle it exactly once.
#[tokio::test]
async fn answer_and_skip_race_settles_once() {
    let asked =
        Asked::idle(Script::which_db(AfterAsk::Answer).kernel(), "skip-race")
            .await;
    let (answered, skipped) =
        tokio::join!(asked.answer(0, "postgres"), asked.dismiss(&[0]));
    let skipped = skipped.expect("dismiss");
    match &answered {
        Ok(PromptOutcome::Ran(_)) => assert_eq!(skipped, 0),
        result => {
            assert!(is_closed(result, 0), "{:?}", result.as_ref().err());
            assert_eq!(skipped, 1);
        }
    }
    let settlements = asked
        .replies()
        .iter()
        .flat_map(|reply| reply.keys().collect::<Vec<_>>())
        .filter(|settled| *settled == asked.key(0))
        .count();
    assert_eq!(settlements, 1);
}

/// Stopping a Run still writes back its pending skips.
#[tokio::test]
async fn stopped_run_writes_back_a_pending_skip() {
    let release = Arc::new(Notify::new());
    let script = Script::which_db(AfterAsk::Hold(Arc::clone(&release)));
    let (asked, run) = Asked::held(script.kernel(), "skip-stop").await;
    assert_eq!(asked.dismiss(&[0]).await.expect("dismiss"), 1);
    asked
        .test
        .kernel
        .cancel_session(&asked.session)
        .expect("cancel");
    let _ = run.await.expect("join");
    assert_eq!(asked.replies(), vec![asked.skip(0)]);
    assert!(is_closed(&asked.answer(0, "late").await, 0));
}

/// An idle plain prompt skips the open questions before the model reads it.
#[tokio::test]
async fn plain_prompt_skips_open_questions() {
    let asked = Asked::idle(
        Script::which_db(AfterAsk::Answer).kernel(),
        "supersede-idle",
    )
    .await;
    assert!(matches!(
        asked.say("move on").await,
        Ok(PromptOutcome::Ran(_))
    ));
    assert_eq!(asked.replies(), vec![asked.skip(0)]);
    let requests = asked.requests();
    assert!(requests.last().is_some_and(|request| {
        request
            .messages
            .iter()
            .any(|message| QuestionReply::try_from(&message.content).is_ok())
    }));
}

/// A busy plain prompt skips the open questions without an extra model call.
#[tokio::test]
async fn busy_prompt_skips_open_questions_without_a_model_call() {
    let release = Arc::new(Notify::new());
    let script = Script::which_db(AfterAsk::Hold(Arc::clone(&release)));
    let (asked, run) = Asked::held(script.kernel(), "supersede-busy").await;
    assert!(matches!(
        asked.say("move on").await,
        Ok(PromptOutcome::Queued(_))
    ));
    release.notify_one();
    run.await.expect("join").expect("Run");
    // Ask, the held answer, then one Turn for the follow-up prompt.
    assert_eq!(asked.test.model.request_count(), 3);
    assert_eq!(asked.replies(), vec![asked.skip(0)]);
}

/// The skip a prompt causes enters history before that prompt, so the
/// prompt stays the model's latest user input.
#[tokio::test]
async fn superseding_skip_precedes_the_prompt() {
    let asked = Asked::idle(
        Script::which_db(AfterAsk::Answer).kernel(),
        "supersede-order",
    )
    .await;
    asked.say("move on").await.expect("prompt");
    let transcript = asked
        .test
        .kernel
        .session_transcript(&asked.session)
        .expect("transcript");
    let skip = transcript
        .iter()
        .position(|message| QuestionReply::try_from(&message.content).is_ok());
    let prompt = transcript
        .iter()
        .position(|message| text(&message.content) == "move on");
    assert!(skip.is_some() && skip < prompt, "{skip:?} {prompt:?}");
}

/// A prompt withdrawn before it reaches history skips nothing.
#[tokio::test]
async fn withdrawn_prompt_leaves_questions_open() {
    let release = Arc::new(Notify::new());
    let script = Script::which_db(AfterAsk::Hold(Arc::clone(&release)));
    let (asked, run) =
        Asked::held(script.kernel(), "supersede-withdrawn").await;
    let (request, receiver) = asked.request(RunInput::Text("move on".into()));
    assert!(matches!(
        asked.test.kernel.prompt(request).await,
        Ok(PromptOutcome::Queued(_))
    ));
    asked
        .test
        .kernel
        .cancel_session(&asked.session)
        .expect("cancel");
    assert!(matches!(
        receiver.await,
        Ok(Err(KernelError::PromptCancelled))
    ));
    let _ = run.await.expect("join");
    assert!(asked.replies().is_empty());
    assert!(matches!(
        asked.answer(0, "postgres").await,
        Ok(PromptOutcome::Ran(_))
    ));
}
