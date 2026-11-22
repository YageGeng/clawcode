//! The Kernel snapshots a top-level Session's workspace before each Run and
//! after it, before the Run ends, through the injected `RunCheckpoints`; it
//! removes a deleted Session's snapshots the same way.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use extension::StaticExtensionFactory;
use futures::{Stream, stream};
use kernel::{
    CheckpointPoint, CheckpointStage, EventSink, Kernel, KernelFactory, Model,
    ModelError, ModelFactory, RunCheckpoints,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, IdGenerator, IdKind, MessageId,
    ModelFailure, ModelFinal, ModelProfile, ModelRequest,
    ModelRetryDisposition, ModelStreamEvent, RetryPolicy, RunId, RunRequest,
    SessionId, StopReason, TimestampMs,
};
use store::{Clock, JsonlStoreFactory, SessionCreateOptions};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

type TestModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// The profile of the scripted model.
static PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("checkpoint".to_string())
        .display_name("Checkpoint fixture".to_string())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .build()
});

/// Advances one millisecond per observation.
struct StepClock(AtomicU64);

impl Clock for StepClock {
    /// Returns the next millisecond.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// Readable sequential identifiers.
struct SequentialIds(AtomicU64);

impl IdGenerator for SequentialIds {
    /// Returns the next identifier with its domain prefix.
    fn next(&self, kind: IdKind) -> String {
        let value = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        format!("{}-{value}", kind.prefix())
    }
}

/// One scripted provider response.
enum Response {
    /// A complete text answer.
    Text(&'static str),
    /// A request failure that is never retried.
    Failure,
    /// A stream that never yields, until the Run is cancelled.
    Hang,
}

/// Answers from a script.
struct ScriptModel(Mutex<VecDeque<Response>>);

#[async_trait]
impl Model for ScriptModel {
    /// Returns the fixture profile.
    fn profile(&self) -> &ModelProfile {
        &PROFILE
    }

    /// Needs no provider.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Returns the next scripted response.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        let response = self
            .0
            .lock()
            .expect("script")
            .pop_front()
            .expect("scripted response");
        match response {
            Response::Text(text) => Ok(Box::pin(stream::iter(
                [
                    ModelStreamEvent::TextDelta(text.to_string()),
                    ModelStreamEvent::Finished(
                        ModelFinal::builder()
                            .stop_reason(StopReason::EndTurn)
                            .usage(protocol::ModelUsage::default())
                            .build(),
                    ),
                ]
                .into_iter()
                .map(Ok),
            ))),
            Response::Failure => Err(ModelError::Request(
                ModelFailure::builder()
                    .summary("bad request".to_string())
                    .status(Some(400))
                    .retry_disposition(ModelRetryDisposition::NonRetryable)
                    .build(),
            )),
            Response::Hang => Ok(Box::pin(stream::pending())),
        }
    }
}

/// Hands out the shared script model.
struct StaticModelFactory(Arc<dyn Model>);

impl ModelFactory for StaticModelFactory {
    /// Clones the shared model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0))
    }
}

/// Records events and wakes waiters on each.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<AgentEvent>>,
    changed: Notify,
}

impl RecordingSink {
    /// Whether a `RunEnd` was emitted so far.
    fn ended(&self) -> bool {
        self.events.lock().expect("events").iter().any(|event| {
            matches!(event.payload, AgentEventPayload::RunEnd { .. })
        })
    }

    /// Waits until the Run has started.
    async fn run_started(&self) {
        loop {
            let notified = self.changed.notified();
            if self.events.lock().expect("events").iter().any(|event| {
                matches!(event.payload, AgentEventPayload::RunStart { .. })
            }) {
                return;
            }
            notified.await;
        }
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    /// Records the event and wakes waiters.
    async fn emit(&self, event: AgentEvent) -> Result<(), kernel::SinkError> {
        self.events.lock().expect("events").push(event);
        self.changed.notify_waiters();
        Ok(())
    }
}

/// One snapshot request as the fixture saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Captured {
    stage: Stage,
    session_id: SessionId,
    run_id: RunId,
    anchor: Option<MessageId>,
    /// Whether the Run had already emitted `RunEnd` when it was asked for.
    after_run_end: bool,
}

/// A comparable copy of `CheckpointStage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Start,
    End,
}

/// Records snapshot and removal requests; the start can be made slow.
#[derive(Default)]
struct RecordingCheckpoints {
    captured: Mutex<Vec<Captured>>,
    removed: Mutex<Vec<(PathBuf, SessionId)>>,
    /// How long a start snapshot takes.
    start_delay: Mutex<Option<Duration>>,
    /// The sink of the Run, to tell whether it already ended.
    sink: Mutex<Option<Arc<RecordingSink>>>,
}

#[async_trait]
impl RunCheckpoints for RecordingCheckpoints {
    /// Records the request, after the configured start delay.
    async fn capture(&self, point: CheckpointPoint<'_>) -> Result<(), String> {
        let stage = match point.stage {
            CheckpointStage::Start => Stage::Start,
            CheckpointStage::End => Stage::End,
        };
        let delay = *self.start_delay.lock().expect("delay");
        if let (Stage::Start, Some(delay)) = (stage, delay) {
            tokio::time::sleep(delay).await;
        }
        let after_run_end = self
            .sink
            .lock()
            .expect("sink")
            .as_ref()
            .is_some_and(|sink| sink.ended());
        self.captured.lock().expect("captured").push(Captured {
            stage,
            session_id: point.session_id.clone(),
            run_id: point.run_id.clone(),
            anchor: point.anchor.cloned(),
            after_run_end,
        });
        Ok(())
    }

    /// Records the removal.
    async fn remove(
        &self,
        cwd: &Path,
        session_id: &SessionId,
    ) -> Result<(), String> {
        self.removed
            .lock()
            .expect("removed")
            .push((cwd.to_path_buf(), session_id.clone()));
        Ok(())
    }
}

impl RecordingCheckpoints {
    /// Stages recorded so far, in order.
    fn stages(&self) -> Vec<Stage> {
        self.captured
            .lock()
            .expect("captured")
            .iter()
            .map(|captured| captured.stage)
            .collect()
    }

    /// The end snapshot request, if any.
    fn end(&self) -> Option<Captured> {
        self.captured
            .lock()
            .expect("captured")
            .iter()
            .find(|captured| captured.stage == Stage::End)
            .cloned()
    }
}

/// One kernel over a script with recording checkpoints and one Session.
struct Fixture {
    kernel: Arc<Kernel>,
    checkpoints: Arc<RecordingCheckpoints>,
    session_id: SessionId,
    cwd: PathBuf,
    _root: tempfile::TempDir,
}

impl Fixture {
    /// Builds the kernel with `responses` and creates the Session.
    async fn new(responses: Vec<Response>) -> Self {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("workspace");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(10_000)));
        let checkpoints = Arc::new(RecordingCheckpoints::default());
        let model: Arc<dyn Model> =
            Arc::new(ScriptModel(Mutex::new(VecDeque::from(responses))));
        let kernel = Arc::new(
            KernelFactory::builder()
                .model_factory(Arc::new(StaticModelFactory(model)))
                .tool_factory(Arc::new(BuiltinToolFactory::new()))
                .store_factory(Arc::new(JsonlStoreFactory::new(
                    root.path(),
                    Arc::clone(&clock),
                )))
                .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                    root.path().join("config"),
                    protocol::PromptPolicy::default(),
                )))
                .extension_factory(Arc::new(StaticExtensionFactory::default()))
                .clock(Arc::clone(&clock))
                .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
                .retry_policy(
                    RetryPolicy::builder()
                        .enabled(false)
                        .max_retries(0)
                        .base_delay_ms(1_000)
                        .max_retry_delay_ms(1_000)
                        .build(),
                )
                .checkpoints(Some(
                    Arc::clone(&checkpoints) as Arc<dyn RunCheckpoints>
                ))
                .build()
                .build()
                .expect("kernel"),
        );
        let session_id = SessionId::try_from("session-checkpoint").expect("id");
        kernel
            .create_session(
                SessionCreateOptions::builder()
                    .session_id(session_id.clone())
                    .cwd(cwd.clone())
                    .build(),
            )
            .await
            .expect("session");
        Self {
            kernel,
            checkpoints,
            session_id,
            cwd,
            _root: root,
        }
    }

    /// Runs one prompt in `session_id` with a sink the checkpoints can see.
    async fn run_in(
        &self,
        session_id: &SessionId,
    ) -> Result<protocol::RunResult, kernel::KernelError> {
        let sink = Arc::new(RecordingSink::default());
        *self.checkpoints.sink.lock().expect("sink") = Some(Arc::clone(&sink));
        self.kernel
            .run(
                RunRequest {
                    session_id: session_id.clone(),
                    input: "hello".into(),
                },
                sink,
            )
            .await
    }
}

/// A finished Run is snapshotted at its start and at its end, both tagged
/// with the Run; only the end names a reply.
#[tokio::test]
async fn run_captures_start_then_end() {
    let fixture = Fixture::new(vec![Response::Text("done")]).await;
    let result = fixture.run_in(&fixture.session_id).await.expect("run");

    assert_eq!(fixture.checkpoints.stages(), vec![Stage::Start, Stage::End]);
    let captured = fixture
        .checkpoints
        .captured
        .lock()
        .expect("captured")
        .clone();
    assert!(captured.iter().all(|point| point.run_id == result.run_id
        && point.session_id == fixture.session_id));
    assert_eq!(captured[0].anchor, None);
}

/// The end names the Run's last assistant message, the one a client shows the change under.
#[tokio::test]
async fn end_capture_names_the_last_assistant_message() {
    let fixture = Fixture::new(vec![Response::Text("done")]).await;
    let result = fixture.run_in(&fixture.session_id).await.expect("run");

    let reply = result
        .messages
        .iter()
        .rev()
        .find(|message| {
            matches!(
                message.content,
                protocol::MessageContent::Assistant { .. }
            )
        })
        .expect("assistant reply");
    assert_eq!(
        fixture.checkpoints.end().expect("end").anchor.as_ref(),
        Some(&reply.identity.message_id)
    );
}

/// The end snapshot is written before the Run reports its end, so a client
/// refreshing on `RunEnd` already finds it.
#[tokio::test]
async fn end_is_captured_before_run_end() {
    let fixture = Fixture::new(vec![Response::Text("done")]).await;
    fixture.run_in(&fixture.session_id).await.expect("run");

    assert!(!fixture.checkpoints.end().expect("end").after_run_end);
}

/// A Run cancelled while the model streams still gets its end snapshot.
#[tokio::test]
async fn cancelled_run_still_captures_its_end() {
    let fixture = Fixture::new(vec![Response::Hang]).await;
    let sink = Arc::new(RecordingSink::default());
    *fixture.checkpoints.sink.lock().expect("sink") = Some(Arc::clone(&sink));
    let kernel = Arc::clone(&fixture.kernel);
    let session_id = fixture.session_id.clone();
    let run_sink = Arc::clone(&sink);
    let run = tokio::spawn(async move {
        kernel
            .run(
                RunRequest {
                    session_id,
                    input: "hello".into(),
                },
                run_sink,
            )
            .await
    });
    sink.run_started().await;
    fixture
        .kernel
        .cancel_session(&fixture.session_id)
        .expect("cancel");
    let _ = run.await.expect("join run");

    assert_eq!(fixture.checkpoints.stages(), vec![Stage::Start, Stage::End]);
}

/// A Run whose model request fails still gets its end snapshot.
#[tokio::test]
async fn failed_run_still_captures_its_end() {
    let fixture = Fixture::new(vec![Response::Failure]).await;
    let result = fixture.run_in(&fixture.session_id).await.expect("run");

    assert!(matches!(
        result.turns.last().map(|turn| &turn.outcome),
        Some(protocol::TurnOutcome::Failed { .. })
    ));
    assert_eq!(fixture.checkpoints.stages(), vec![Stage::Start, Stage::End]);
}

/// A start snapshot that overruns its 5 second budget is abandoned: the Run
/// goes on, and without a start there is no end to compare it with.
#[tokio::test(start_paused = true)]
async fn start_timeout_skips_the_end() {
    let fixture = Fixture::new(vec![Response::Text("done")]).await;
    *fixture.checkpoints.start_delay.lock().expect("delay") =
        Some(Duration::from_secs(6));

    fixture.run_in(&fixture.session_id).await.expect("run");

    assert!(fixture.checkpoints.stages().is_empty());
}

/// Runs of a Session another Session owns, a subagent's, are not snapshotted.
#[tokio::test]
async fn subagent_runs_are_not_captured() {
    let fixture = Fixture::new(vec![Response::Text("done")]).await;
    let child = fixture
        .kernel
        .create_owned_session(
            kernel::OwnedSessionRequest::builder()
                .ownership(protocol::SessionOwnership {
                    session_id: fixture.session_id.clone(),
                    name: protocol::AgentName::try_from("child").expect("name"),
                    nickname: "child".into(),
                })
                .title(
                    protocol::SessionTitle::try_from("child").expect("title"),
                )
                .model(None)
                .build(),
        )
        .await
        .expect("child");

    fixture.run_in(&child).await.expect("run");

    assert!(fixture.checkpoints.stages().is_empty());
}

/// Deleting a Session removes its snapshots in its workspace.
#[tokio::test]
async fn deleting_a_session_removes_its_checkpoints() {
    let fixture = Fixture::new(Vec::new()).await;
    fixture
        .kernel
        .delete_session(&fixture.session_id)
        .await
        .expect("delete");

    assert_eq!(
        *fixture.checkpoints.removed.lock().expect("removed"),
        vec![(fixture.cwd.clone(), fixture.session_id.clone())]
    );
}

/// A client prompt cancelled while its run's start snapshot is taken never
/// enters history; the run's end must not borrow the previous run's reply
/// as its anchor.
#[tokio::test]
async fn cancel_before_the_question_leaves_no_anchor() {
    let fixture =
        Fixture::new(vec![Response::Text("done"), Response::Hang]).await;
    fixture
        .run_in(&fixture.session_id)
        .await
        .expect("first run");
    *fixture.checkpoints.start_delay.lock().expect("delay") =
        Some(Duration::from_millis(300));

    let sink = Arc::new(RecordingSink::default());
    *fixture.checkpoints.sink.lock().expect("sink") = Some(Arc::clone(&sink));
    let kernel = Arc::clone(&fixture.kernel);
    let session_id = fixture.session_id.clone();
    let run_sink = Arc::clone(&sink);
    // A client prompt: one an insertion notice reports on, which a
    // cancellation keeps out of history.
    let (notice, _insertion) = kernel::InsertionNotice::new();
    let run = tokio::spawn(async move {
        kernel
            .run_with_insertion(
                RunRequest {
                    session_id,
                    input: "again".into(),
                },
                run_sink,
                Some(notice),
            )
            .await
    });
    sink.run_started().await;
    fixture
        .kernel
        .cancel_session(&fixture.session_id)
        .expect("cancel");
    let _ = run.await.expect("join run");

    let last_end = fixture
        .checkpoints
        .captured
        .lock()
        .expect("captured")
        .iter()
        .rev()
        .find(|captured| captured.stage == Stage::End)
        .cloned()
        .expect("second end");
    assert_ne!(last_end, fixture.checkpoints.end().expect("first end"));
    assert_eq!(last_end.anchor, None);
}
