use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use extension::{
    BeforeAgentStartPoint, ExtensionContext, ExtensionError, ExtensionHandler,
    ExtensionModule, ExtensionRegistrar, InputPoint, MessageEndPoint,
    MessageStartPoint, StaticExtensionFactory,
};
use kernel::{
    AnswerOutcome, InsertionNotice, KernelError, ModelError, SessionEventSinks,
    SubmitRequest, WhenBusy,
};
use protocol::{
    AgentEventPayload, AgentOutcome, BeforeAgentStartEvent,
    BeforeAgentStartResult, ContentBlock, ExtensionDescriptor, ExtensionId,
    InputEvent, InputResult, MessageContent, MessageEndEvent, MessageId,
    MessageStartEvent, ModelRequest, RunRequest, RunResult, SessionId,
    SlashCommandMessage, SlashCommandStatus, StaticExtensionRegistration,
    TraceId,
};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::fixture::{Route, TestKernel, request, text};

type Insertion = oneshot::Receiver<Result<MessageId, KernelError>>;

/// Gives a spawned Run time to reach the Session gate before the test acts.
const SETTLE: Duration = Duration::from_millis(100);

/// Builds the trace id shared by direct Kernel Runs in these tests.
fn trace() -> TraceId {
    TraceId::try_from("trace-alignment").expect("trace")
}

/// Starts one notice-carrying Run on a detached task, as the ACP prompt handler does.
fn start(
    test: &TestKernel,
    session: &SessionId,
    input: &str,
) -> (JoinHandle<Result<RunResult, KernelError>>, Insertion) {
    let (notice, receiver) = InsertionNotice::new();
    let kernel = test.kernel.clone();
    let sink = test.sinks.sink(session);
    let run_request = RunRequest {
        session_id: session.clone(),
        input: input.into(),
    };
    let run = tokio::spawn(async move {
        kernel
            .run_traced(run_request, sink, trace(), Some(notice))
            .await
    });
    (run, receiver)
}

/// Reports whether a provider request is the dedicated compaction summary protocol.
fn is_summary_request(request: &ModelRequest) -> bool {
    request.messages.first().is_some_and(|message| {
        matches!(
            &message.content,
            MessageContent::System { blocks } if blocks.iter().any(|block| matches!(
                block,
                ContentBlock::Text { text } if text.contains("context summarization assistant")
            ))
        )
    })
}

/// Routes "first" to a hanging stream so it holds the gate until cancelled.
fn hang_first(request: &ModelRequest) -> Route {
    if request
        .messages
        .last()
        .is_some_and(|message| text(&message.content) == "first")
    {
        Route::Hang
    } else {
        Route::answer("answer")
    }
}

/// Starts "/compact" so a direct command holds the gate while no Run is active.
async fn hold_gate_with_compaction(
    test: &TestKernel,
    session: &SessionId,
) -> JoinHandle<Result<RunResult, KernelError>> {
    let compact = start_compaction(test, session).await;
    let summaries = summary_requests(test);
    test.wait_until(|| summary_requests(test) > summaries).await;
    compact
}

/// Counts dedicated compaction summary requests sent to the model so far.
fn summary_requests(test: &TestKernel) -> usize {
    test.model
        .requests
        .lock()
        .expect("requests")
        .iter()
        .filter(|request| is_summary_request(request))
        .count()
}

/// Builds compactable history, then starts "/compact" without waiting for it.
async fn start_compaction(
    test: &TestKernel,
    session: &SessionId,
) -> JoinHandle<Result<RunResult, KernelError>> {
    // Two exchanges larger than the default 20k-token recent window give
    // manual compaction an older Turn to summarize.
    for _ in 0..2 {
        test.kernel
            .run(
                RunRequest {
                    session_id: session.clone(),
                    input: "x".repeat(100_000).into(),
                },
                test.sinks.sink(session),
            )
            .await
            .expect("history run");
    }
    let (compact, _receiver) = start(test, session, "/compact");
    compact
}

/// Routes summary requests to a hang and every other request to an answer.
fn hang_summary(request: &ModelRequest) -> Route {
    if is_summary_request(request) {
        Route::Hang
    } else {
        Route::answer("answer")
    }
}

/// Returns persisted user texts for the Session.
fn transcript(
    test: &TestKernel,
    session: &SessionId,
) -> Vec<protocol::AgentMessage> {
    test.kernel.session_transcript(session).expect("transcript")
}

/// Builds one extension module from a single handler registration.
struct HookModule<F>(F);

impl<F> ExtensionModule for HookModule<F>
where
    F: Fn(&mut ExtensionRegistrar) -> Result<(), ExtensionError> + Send + Sync,
{
    /// Identifies the alignment test extension.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("alignment-hooks").expect("id"),
            name: "Alignment hooks".into(),
            version: "1".into(),
        }
    }

    /// Registers the test's handler under the module identity.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        (self.0)(registrar)
    }
}

/// Wraps a registration closure into a static extension factory.
fn hooks<F>(register: F) -> Arc<dyn extension::ExtensionFactory>
where
    F: Fn(&mut ExtensionRegistrar) -> Result<(), ExtensionError>
        + Clone
        + Send
        + Sync
        + 'static,
{
    Arc::new(StaticExtensionFactory::new(
        StaticExtensionRegistration::default(),
        vec![Arc::new(move || {
            Ok(Arc::new(HookModule(register.clone()))
                as Arc<dyn ExtensionModule>)
        })],
    ))
}

/// Input handler with a fixed result or failure.
#[derive(Clone)]
struct FixedInput(Option<InputResult>);

#[async_trait]
impl ExtensionHandler<InputPoint> for FixedInput {
    /// Returns the scripted input result, or fails when none is scripted.
    async fn handle(
        &self,
        _event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        self.0.clone().ok_or_else(|| {
            ExtensionError::Handler("input fixture failure".into())
        })
    }
}

/// Pre-insertion hook that ignores cancellation until the test releases it.
#[derive(Clone)]
struct BlockingInsertion {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ExtensionHandler<BeforeAgentStartPoint> for BlockingInsertion {
    /// Signals entry and blocks the Run after admission but before insertion.
    async fn handle(
        &self,
        _event: &BeforeAgentStartEvent,
        _context: &ExtensionContext,
    ) -> Result<BeforeAgentStartResult, ExtensionError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(BeforeAgentStartResult::default())
    }
}

#[async_trait]
impl ExtensionHandler<MessageStartPoint> for BlockingInsertion {
    /// Pauses the user message after the early cancellation check.
    async fn handle(
        &self,
        event: &MessageStartEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        if matches!(event.message.content, MessageContent::User { .. }) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

#[async_trait]
impl ExtensionHandler<MessageEndPoint> for BlockingInsertion {
    /// Pauses the last message hook before user-message persistence.
    async fn handle(
        &self,
        event: &MessageEndEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<protocol::AgentMessage>, ExtensionError> {
        if matches!(event.message.content, MessageContent::User { .. }) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(None)
    }
}

#[async_trait]
impl ExtensionHandler<InputPoint> for BlockingInsertion {
    /// Handles input only after cancellation may have reached the owning Run.
    async fn handle(
        &self,
        _event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(InputResult::Handled)
    }
}

/// Creates a marker path whose existence proves a shell command ran.
fn marker(test: &TestKernel, name: &str) -> PathBuf {
    test.root.path().join(name)
}

/// The notice reports the id of the first persisted user message.
#[tokio::test]
async fn insertion_reports_persisted_user_message_id() {
    let test = TestKernel::new(|_| Route::answer("hi"));
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "hello");
    let id = receiver.await.expect("sent").expect("inserted");
    run.await.expect("join").expect("run");
    let user = transcript(&test, &session)
        .into_iter()
        .find(|message| matches!(message.content, MessageContent::User { .. }))
        .expect("user message");
    assert_eq!(user.identity.message_id, id);
}

/// A direct command is inserted by its persisted invocation message.
#[tokio::test]
async fn slash_command_insertion_uses_invocation_message() {
    let test = TestKernel::new(|_| Route::answer("hi"));
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "/session");
    let id = receiver.await.expect("sent").expect("inserted");
    run.await.expect("join").expect("run");
    let invocation = transcript(&test, &session)
        .into_iter()
        .find(|message| {
            matches!(
                message.content,
                MessageContent::SlashCommand {
                    message: SlashCommandMessage::Invocation { .. }
                }
            )
        })
        .expect("invocation");
    assert_eq!(invocation.identity.message_id, id);
}

/// A busy direct command is persisted, so it counts as inserted and reports failure via its output message.
#[tokio::test]
async fn busy_slash_command_is_inserted_with_failed_output() {
    let test = TestKernel::new(hang_first);
    let session = test.create("owner").await;
    let (run_a, receiver_a) = start(&test, &session, "first");
    receiver_a.await.expect("sent").expect("first inserted");
    let (run_b, receiver_b) = start(&test, &session, "/session");
    let id = receiver_b
        .await
        .expect("sent")
        .expect("busy command inserted");
    let result = run_b.await.expect("join").expect("busy command");
    assert_eq!(result.messages[0].identity.message_id, id);
    assert!(matches!(
        &result.messages[1].content,
        MessageContent::SlashCommand {
            message: SlashCommandMessage::Output(output)
        } if output.status == SlashCommandStatus::Failed
    ));
    assert!(!test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::SlashCommandStart { run_id, .. }
            | AgentEventPayload::SlashCommandEnd { run_id, .. }
            if *run_id == result.run_id
    )));
    test.kernel.cancel_session(&session).expect("cancel");
    run_a.await.expect("join").expect("first run");
}

/// Input consumed by an extension is inserted as a live-only user message.
#[tokio::test]
async fn extension_handled_input_inserts_live_only_message() {
    let test = TestKernel::with_extensions(
        |_| Route::answer("unused"),
        hooks(|registrar| {
            registrar
                .on::<InputPoint, _>(FixedInput(Some(InputResult::Handled)))
        }),
    );
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "handled");
    let id = receiver.await.expect("sent").expect("inserted");
    run.await.expect("join").expect("run");
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::MessageEnd { message }
            if message.identity.message_id == id
                && matches!(message.content, MessageContent::User { .. })
    )));
    assert!(
        transcript(&test, &session)
            .iter()
            .all(|message| message.identity.message_id != id)
    );
}

/// A session-wide cancel rejects a client prompt still waiting for the gate.
#[tokio::test]
async fn cancel_rejects_prompt_waiting_for_gate() {
    let test = TestKernel::new(hang_first);
    let session = test.create("owner").await;
    let (run_a, receiver_a) = start(&test, &session, "first");
    receiver_a.await.expect("sent").expect("first inserted");
    let (run_b, receiver_b) = start(&test, &session, "second");
    tokio::time::sleep(SETTLE).await;
    test.kernel.cancel_session(&session).expect("cancel");
    assert!(matches!(
        receiver_b.await.expect("sent"),
        Err(KernelError::PromptCancelled)
    ));
    assert!(matches!(
        run_b.await.expect("join"),
        Err(KernelError::PromptCancelled)
    ));
    run_a.await.expect("join").expect("first run");
    assert_eq!(test.user_texts(&session), vec!["first".to_string()]);
}

/// A submission whose parent was already cancelled reports Cancelled, not a failure.
#[tokio::test]
async fn submit_maps_prompt_cancelled_to_cancelled_outcome() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("owner").await;
    let parent = CancellationToken::new();
    parent.cancel();
    let submission = SubmitRequest::builder()
        .session_id(session.clone())
        .content(vec![ContentBlock::Text {
            text: "report".into(),
        }])
        .when_busy(WhenBusy::FollowUp)
        .cancellation(Some(parent))
        .build();
    let answer = test.kernel.submit(submission).await.expect("answer");
    assert!(matches!(answer.outcome, AnswerOutcome::Cancelled));
    assert!(test.user_texts(&session).is_empty());
}

/// Neither prefixed shell form may start after cancellation while waiting for the gate.
#[tokio::test]
async fn cancel_rejects_waiting_user_bash_without_spawning() {
    let test = TestKernel::new(hang_first);
    let session = test.create("owner").await;
    let (run_a, receiver_a) = start(&test, &session, "first");
    receiver_a.await.expect("sent").expect("first inserted");
    let bang = marker(&test, "bang");
    let excluded = marker(&test, "excluded");
    let (run_bang, receiver_bang) =
        start(&test, &session, &format!("!touch {}", bang.display()));
    let (run_excluded, receiver_excluded) =
        start(&test, &session, &format!("!!touch {}", excluded.display()));
    tokio::time::sleep(SETTLE).await;
    test.kernel.cancel_session(&session).expect("cancel");
    for receiver in [receiver_bang, receiver_excluded] {
        assert!(matches!(
            receiver.await.expect("sent"),
            Err(KernelError::PromptCancelled)
        ));
    }
    for run in [run_bang, run_excluded] {
        assert!(matches!(
            run.await.expect("join"),
            Err(KernelError::PromptCancelled)
        ));
    }
    run_a.await.expect("join").expect("first run");
    tokio::time::sleep(SETTLE).await;
    assert!(!bang.exists());
    assert!(!excluded.exists());
}

/// Closing rejects waiting shell prompts through the same admission path.
#[tokio::test]
async fn close_rejects_waiting_user_bash_without_spawning() {
    let test = TestKernel::new(hang_first);
    let session = test.create("owner").await;
    let (run_a, receiver_a) = start(&test, &session, "first");
    receiver_a.await.expect("sent").expect("first inserted");
    let bang = marker(&test, "closed");
    let (run_bang, receiver_bang) =
        start(&test, &session, &format!("!touch {}", bang.display()));
    tokio::time::sleep(SETTLE).await;
    test.kernel.close_session(&session).await.expect("close");
    assert!(matches!(
        receiver_bang.await.expect("sent"),
        Err(KernelError::PromptCancelled)
    ));
    assert!(matches!(
        run_bang.await.expect("join"),
        Err(KernelError::PromptCancelled)
    ));
    run_a.await.expect("join").expect("first run");
    assert!(!bang.exists());
}

/// A shell prompt is inserted when it starts and persists its result under the same id.
#[tokio::test]
async fn user_bash_inserts_at_start_and_persists_same_id() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "!sleep 1; echo done");
    let id = tokio::time::timeout(Duration::from_millis(500), receiver)
        .await
        .expect("inserted before the command ends")
        .expect("sent")
        .expect("inserted");
    run.await.expect("join").expect("run");
    let bash = transcript(&test, &session)
        .into_iter()
        .find(|message| {
            matches!(message.content, MessageContent::BashExecution { .. })
        })
        .expect("bash message");
    assert_eq!(bash.identity.message_id, id);
}

/// Returns the user-bash terminal events emitted for a Session.
fn user_bash_finishes(
    test: &TestKernel,
    session: &SessionId,
) -> Vec<(AgentOutcome, bool)> {
    test.sinks
        .payloads(session)
        .into_iter()
        .filter_map(|payload| match payload {
            AgentEventPayload::UserBashFinished {
                outcome,
                result_emitted,
                ..
            } => Some((outcome, result_emitted)),
            _ => None,
        })
        .collect()
}

/// A completed shell emits its result before exactly one successful finish.
#[tokio::test]
async fn user_bash_success_settles_once_after_result() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "!echo hi");
    let id = receiver.await.expect("sent").expect("inserted");
    run.await.expect("join").expect("run");
    let payloads = test.sinks.payloads(&session);
    let started = payloads
        .iter()
        .position(|payload| matches!(
            payload,
            AgentEventPayload::UserBashStarted { message_id, .. } if *message_id == id
        ))
        .expect("started");
    let result = payloads
        .iter()
        .position(|payload| matches!(
            payload,
            AgentEventPayload::MessageEnd { message } if message.identity.message_id == id
        ))
        .expect("result");
    let finished = payloads
        .iter()
        .position(|payload| {
            matches!(payload, AgentEventPayload::UserBashFinished { .. })
        })
        .expect("finished");
    assert!(started < result && result < finished);
    assert_eq!(
        user_bash_finishes(&test, &session),
        vec![(AgentOutcome::Succeeded, true)]
    );
}

/// Stopping a running shell persists its partial result and finishes once as cancelled.
#[tokio::test]
async fn user_bash_cancel_settles_once_as_cancelled() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "!sleep 5");
    receiver.await.expect("sent").expect("inserted");
    tokio::time::sleep(SETTLE).await;
    test.kernel.cancel_session(&session).expect("cancel");
    run.await
        .expect("join")
        .expect("cancelled shell still settles");
    assert_eq!(
        user_bash_finishes(&test, &session),
        vec![(AgentOutcome::Cancelled, true)]
    );
}

/// An executor failure after insertion finishes once without a result message.
#[tokio::test]
async fn user_bash_executor_error_finishes_without_result() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let cwd = test.root.path().join("removed");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    let session = SessionId::try_from("owner").expect("id");
    test.kernel
        .create_session(store::SessionCreateOptions {
            session_id: session.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create");
    std::fs::remove_dir(&cwd).expect("remove cwd");
    let (run, receiver) = start(&test, &session, "!echo hi");
    receiver
        .await
        .expect("sent")
        .expect("inserted before execution");
    run.await.expect("join").expect_err("executor failure");
    let finishes = user_bash_finishes(&test, &session);
    assert_eq!(finishes.len(), 1);
    assert!(matches!(finishes[0], (AgentOutcome::Failed { .. }, false)));
}

/// A provider preflight error propagates before any user message is inserted.
#[tokio::test]
async fn preflight_failure_rejects_insertion() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    *test.model.preflight_failure.lock().expect("preflight") =
        Some(|| ModelError::Unavailable("provider offline".into()));
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "hello");
    let rejection = receiver.await.expect("sent");
    assert!(
        matches!(&rejection, Err(error) if !matches!(error, KernelError::PromptCancelled))
    );
    run.await
        .expect("join")
        .expect_err("preflight failure ends the Run");
    assert!(test.user_texts(&session).is_empty());
}

/// Input hook errors remain diagnostics and do not reject a prompt.
#[tokio::test]
async fn input_hook_error_reports_diagnostic_and_still_inserts() {
    let test = TestKernel::with_extensions(
        |_| Route::answer("hi"),
        hooks(|registrar| registrar.on::<InputPoint, _>(FixedInput(None))),
    );
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "hello");
    receiver.await.expect("sent").expect("inserted");
    run.await.expect("join").expect("run");
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::ExtensionHandlerFailed { .. }
    )));
}

/// A cancel that lands while a post-admission hook is blocked still rejects the prompt before insertion.
#[tokio::test]
async fn cancel_during_blocked_hook_rejects_before_insertion() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let hook = BlockingInsertion {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let test = TestKernel::with_extensions(
        |_| Route::answer("unused"),
        hooks(move |registrar| {
            registrar.on::<BeforeAgentStartPoint, _>(hook.clone())
        }),
    );
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "hello");
    entered.notified().await;
    test.kernel.cancel_session(&session).expect("cancel");
    release.notify_one();
    assert!(matches!(
        receiver.await.expect("sent"),
        Err(KernelError::PromptCancelled)
    ));
    assert!(matches!(
        run.await.expect("join"),
        Err(KernelError::PromptCancelled)
    ));
    assert!(test.user_texts(&session).is_empty());
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::AgentSettled {
            outcome: AgentOutcome::Cancelled,
            ..
        }
    )));
}

/// Cancellation during message-start must reject acceptance without a visible or stored user message.
#[tokio::test]
async fn cancel_during_message_start_rejects_before_insertion() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let hook = BlockingInsertion {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let test = TestKernel::with_extensions(
        |_| Route::answer("unused"),
        hooks(move |registrar| {
            registrar.on::<MessageStartPoint, _>(hook.clone())
        }),
    );
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "hello");
    entered.notified().await;
    test.kernel.cancel_session(&session).expect("cancel");
    release.notify_one();
    let acceptance = receiver.await.expect("sent");
    let result = run.await.expect("join");
    assert!(matches!(acceptance, Err(KernelError::PromptCancelled)));
    assert!(matches!(result, Err(KernelError::PromptCancelled)));
    assert!(test.user_texts(&session).is_empty());
    assert!(!test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::MessageEnd { message }
            if matches!(message.content, MessageContent::User { .. })
    )));
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::AgentSettled {
            outcome: AgentOutcome::Cancelled,
            ..
        }
    )));
}

/// Cancellation during message-end must be checked again immediately before persistence.
#[tokio::test]
async fn cancel_during_message_end_rejects_before_insertion() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let hook = BlockingInsertion {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let test = TestKernel::with_extensions(
        |_| Route::answer("unused"),
        hooks(move |registrar| {
            registrar.on::<MessageEndPoint, _>(hook.clone())
        }),
    );
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "hello");
    entered.notified().await;
    test.kernel.cancel_session(&session).expect("cancel");
    release.notify_one();
    let acceptance = receiver.await.expect("sent");
    let result = run.await.expect("join");
    assert!(matches!(acceptance, Err(KernelError::PromptCancelled)));
    assert!(matches!(result, Err(KernelError::PromptCancelled)));
    assert!(test.user_texts(&session).is_empty());
    assert!(!test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::MessageEnd { message }
            if matches!(message.content, MessageContent::User { .. })
    )));
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::AgentSettled {
            outcome: AgentOutcome::Cancelled,
            ..
        }
    )));
}

/// Handled input cancelled inside its hook must not create a live-only user insertion.
#[tokio::test]
async fn cancel_during_handled_input_rejects_before_insertion() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let hook = BlockingInsertion {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let test = TestKernel::with_extensions(
        |_| Route::answer("unused"),
        hooks(move |registrar| registrar.on::<InputPoint, _>(hook.clone())),
    );
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "hello");
    entered.notified().await;
    test.kernel.cancel_session(&session).expect("cancel");
    release.notify_one();
    let acceptance = receiver.await.expect("sent");
    let result = run.await.expect("join");
    assert!(matches!(acceptance, Err(KernelError::PromptCancelled)));
    assert!(matches!(result, Err(KernelError::PromptCancelled)));
    assert!(test.user_texts(&session).is_empty());
    assert!(!test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::MessageEnd { message }
            if matches!(message.content, MessageContent::User { .. })
    )));
    assert!(test.model.requests.lock().expect("requests").is_empty());
}

/// A cancellation error before insertion, as pre-prompt compaction reports, rejects the prompt.
#[tokio::test]
async fn cancellation_error_before_insertion_rejects_with_prompt_cancelled() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    *test.model.preflight_failure.lock().expect("preflight") =
        Some(|| ModelError::Cancelled);
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "hello");
    assert!(matches!(
        receiver.await.expect("sent"),
        Err(KernelError::PromptCancelled)
    ));
    assert!(matches!(
        run.await.expect("join"),
        Err(KernelError::PromptCancelled)
    ));
    assert!(test.user_texts(&session).is_empty());
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::AgentSettled {
            outcome: AgentOutcome::Cancelled,
            ..
        }
    )));
}

/// A subagent report waiting behind another operation survives the user's stop.
#[tokio::test]
async fn cancel_does_not_reject_waiting_internal_submission() {
    let test = TestKernel::new(hang_summary);
    let session = test.create("owner").await;
    let compact = hold_gate_with_compaction(&test, &session).await;
    let kernel = test.kernel.clone();
    let report = request(&session, "report", WhenBusy::FollowUp);
    let submit = tokio::spawn(async move { kernel.submit(report).await });
    tokio::time::sleep(SETTLE).await;
    test.kernel.cancel_session(&session).expect("cancel");
    compact.await.expect("join").expect("compact");
    let answer = submit.await.expect("join").expect("answer");
    assert!(matches!(answer.outcome, AnswerOutcome::Completed));
    assert!(test.user_texts(&session).contains(&"report".to_string()));
}

/// Internal input interrupted before insertion stays in the transcript.
#[tokio::test]
async fn internal_submission_cancelled_before_insertion_is_persisted() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    *test.model.preflight_failure.lock().expect("preflight") =
        Some(|| ModelError::Cancelled);
    let session = test.create("owner").await;
    let answer = test
        .kernel
        .submit(request(&session, "report", WhenBusy::FollowUp))
        .await
        .expect("answer");
    assert!(matches!(answer.outcome, AnswerOutcome::Cancelled));
    assert_eq!(test.user_texts(&session), vec!["report".to_string()]);
}

/// Returns the status of the command output produced by a direct Slash Command Run.
fn command_status(result: &RunResult) -> SlashCommandStatus {
    result
        .messages
        .iter()
        .find_map(|message| match &message.content {
            MessageContent::SlashCommand {
                message: SlashCommandMessage::Output(output),
            } => Some(output.status),
            _ => None,
        })
        .expect("command output")
}

/// Reports whether a Session saw SlashCommandEnd with this status.
fn ended_with(
    test: &TestKernel,
    session: &SessionId,
    expected: SlashCommandStatus,
) -> bool {
    test.sinks.saw(session, |event| {
        matches!(
            event,
            AgentEventPayload::SlashCommandEnd { status, .. } if *status == expected
        )
    })
}

/// Cancelling or closing during /compact settles the command as cancelled, not failed.
#[tokio::test]
async fn compact_cancel_and_close_end_cancelled() {
    let test = TestKernel::new(hang_summary);
    let cancelled = test.create("cancelled").await;
    let compact = hold_gate_with_compaction(&test, &cancelled).await;
    test.kernel.cancel_session(&cancelled).expect("cancel");
    let result = compact.await.expect("join").expect("compact");
    assert_eq!(command_status(&result), SlashCommandStatus::Cancelled);
    assert!(ended_with(&test, &cancelled, SlashCommandStatus::Cancelled));

    let closed = test.create("closed").await;
    let compact = hold_gate_with_compaction(&test, &closed).await;
    test.kernel.close_session(&closed).await.expect("close");
    let result = compact.await.expect("join").expect("compact");
    assert_eq!(command_status(&result), SlashCommandStatus::Cancelled);
    assert!(ended_with(&test, &closed, SlashCommandStatus::Cancelled));
}

/// A non-cancellation command error still reports Failed with its summary.
#[tokio::test]
async fn compact_provider_error_stays_failed() {
    let test = TestKernel::new(|request| {
        if is_summary_request(request) {
            Route::Events(vec![
                protocol::ModelStreamEvent::TextDelta("partial".into()),
                super::fixture::finished(protocol::StopReason::Error),
            ])
        } else {
            Route::answer("answer")
        }
    });
    let session = test.create("owner").await;
    let compact = start_compaction(&test, &session).await;
    let result = compact.await.expect("join").expect("compact");
    assert_eq!(command_status(&result), SlashCommandStatus::Failed);
    assert!(test.sinks.saw(&session, |event| matches!(
        event,
        AgentEventPayload::SlashCommandEnd {
            status: SlashCommandStatus::Failed,
            message: Some(_),
            ..
        }
    )));
}

/// Returns the persisted shell message of a Session.
fn bash_message(
    test: &TestKernel,
    session: &SessionId,
) -> protocol::BashExecutionMessage {
    transcript(test, session)
        .into_iter()
        .find_map(|message| match message.content {
            MessageContent::BashExecution { bash } => Some(bash),
            _ => None,
        })
        .expect("bash message")
}

/// Live output reaches clients as replacing snapshots before the durable result.
#[tokio::test]
async fn user_bash_streams_output_events() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("owner").await;
    let (run, receiver) =
        start(&test, &session, "!echo one; sleep 0.3; echo two");
    let id = receiver.await.expect("sent").expect("inserted");
    run.await.expect("join").expect("run");
    let payloads = test.sinks.payloads(&session);
    let outputs = payloads
        .iter()
        .enumerate()
        .filter_map(|(index, payload)| match payload {
            AgentEventPayload::UserBashOutput {
                message_id, output, ..
            } if *message_id == id => Some((index, output.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    let (last_index, last_output) = outputs.last().expect("output events");
    // Each snapshot is the whole visible tail, so the last one has both lines.
    assert!(last_output.contains("one") && last_output.contains("two"));
    let end = payloads
        .iter()
        .position(|payload| {
            matches!(
                payload,
                AgentEventPayload::MessageEnd { message }
                    if matches!(message.content, MessageContent::BashExecution { .. })
            )
        })
        .expect("bash MessageEnd");
    assert!(*last_index < end);
}

/// A signal exit keeps its actual cause in the persisted shell result.
#[cfg(unix)]
#[tokio::test]
async fn user_bash_sigterm_survives_persistence() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("owner").await;
    let (run, _receiver) = start(&test, &session, "!echo going; kill -TERM $$");
    run.await.expect("join").expect("run");
    test.kernel
        .close_session(&session)
        .await
        .expect("close Session");
    test.kernel
        .resume_session(session.clone(), test.root.path().to_path_buf())
        .await
        .expect("resume Session");
    let bash = bash_message(&test, &session);
    assert_eq!(bash.result.signal.as_deref(), Some("SIGTERM"));
    assert_eq!(bash.result.exit_code, None);
}

/// Stopping a shell records the SIGKILL the executor sent.
#[cfg(unix)]
#[tokio::test]
async fn stopped_user_bash_preserves_sigkill() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("owner").await;
    let (run, receiver) = start(&test, &session, "!sleep 5");
    receiver.await.expect("sent").expect("inserted");
    tokio::time::sleep(SETTLE).await;
    test.kernel.cancel_session(&session).expect("cancel");
    run.await.expect("join").expect("run");
    let bash = bash_message(&test, &session);
    assert!(bash.result.cancelled);
    assert_eq!(bash.result.signal.as_deref(), Some("SIGKILL"));
}

/// The live start and durable result agree on the absolute working directory.
#[tokio::test]
async fn user_bash_persists_started_cwd() {
    let test = TestKernel::new(|_| Route::answer("unused"));
    let session = test.create("owner").await;
    let (run, _receiver) = start(&test, &session, "!true");
    run.await.expect("join").expect("run");
    let started = test
        .sinks
        .payloads(&session)
        .into_iter()
        .find_map(|payload| match payload {
            AgentEventPayload::UserBashStarted { cwd, .. } => Some(cwd),
            _ => None,
        })
        .expect("started");
    assert!(started.is_absolute());
    assert_eq!(bash_message(&test, &session).cwd, Some(started));
}
