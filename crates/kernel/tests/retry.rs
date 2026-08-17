use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use extension::StaticExtensionFactory;
use futures::{Stream, stream};
use kernel::{
    EventSink, Kernel, KernelFactory, Model, ModelError, ModelFactory,
    RetryClassifier,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, IdGenerator, IdKind, MessageContent,
    ModelFailure, ModelFinal, ModelProfile, ModelRequest,
    ModelRetryDisposition, ModelStreamEvent, ModelUsage, QueueKind,
    RetryPolicy, RunRequest, RunResult, SessionId, StopReason, TimestampMs,
};
use store::{Clock, JsonlStoreFactory, SessionCreateOptions};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

type TestModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// Shared capabilities used by deterministic retry model handles.
static TEST_MODEL_PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("retry".to_string())
        .display_name("Retry fixture".to_string())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .build()
});

/// Supplies deterministic event and persistence timestamps.
struct StepClock(AtomicU64);

impl Clock for StepClock {
    /// Advances one millisecond for every runtime observation.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// Supplies readable identifiers across retry Turns.
struct SequentialIds(AtomicU64);

impl IdGenerator for SequentialIds {
    /// Generates one unique identifier with its domain prefix.
    fn next(&self, kind: IdKind) -> String {
        let value = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        format!("{}-{value}", kind.prefix())
    }
}

/// Scripted model response at the request-acquisition boundary.
enum ScriptedResponse {
    /// Request acquisition fails before a response stream is returned.
    Failure(ModelFailure),
    /// Request acquisition succeeds with a complete sequence of events.
    Events(Vec<ModelStreamEvent>),
}

/// Records model inputs and returns one scripted response per retry attempt.
struct ScriptedModel {
    responses: Mutex<VecDeque<ScriptedResponse>>,
    requests: Mutex<Vec<ModelRequest>>,
    calls: AtomicUsize,
}

impl ScriptedModel {
    /// Builds one successful text response with complete terminal metadata.
    fn text(text: &str) -> ScriptedResponse {
        ScriptedResponse::Events(vec![
            ModelStreamEvent::TextDelta(text.to_string()),
            ModelStreamEvent::Finished(ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: Some("stop".to_string()),
                usage: ModelUsage::builder()
                    .input_tokens(20)
                    .output_tokens(2)
                    .cache_read_tokens(0)
                    .cache_write_tokens(0)
                    .total_tokens(22)
                    .build(),
            }),
        ])
    }
}

#[async_trait]
impl Model for ScriptedModel {
    /// Returns the stable retry fixture capabilities.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that retry tests have no external provider dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Records the active context and returns the next request outcome.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.requests.lock().expect("request lock").push(request);
        match self
            .responses
            .lock()
            .expect("response lock")
            .pop_front()
            .expect("scripted response")
        {
            ScriptedResponse::Failure(failure) => {
                Err(ModelError::Request(failure))
            }
            ScriptedResponse::Events(events) => {
                Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
            }
        }
    }
}

/// Returns the shared scripted model from the kernel factory boundary.
struct StaticModelFactory(Arc<dyn Model>);

impl ModelFactory for StaticModelFactory {
    /// Clones the deterministic model without contacting a provider.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0))
    }
}

/// Records events and wakes tests when retry lifecycle state changes.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<AgentEvent>>,
    changed: Notify,
}

impl RecordingSink {
    /// Waits until the requested retry has been scheduled and returns its delay.
    async fn retry_delay(&self, expected_attempt: u32) -> u64 {
        loop {
            if let Some(delay_ms) =
                self.events.lock().expect("event lock").iter().find_map(
                    |event| match &event.payload {
                        AgentEventPayload::RetryScheduled {
                            attempt,
                            delay_ms,
                            ..
                        } if *attempt == expected_attempt => Some(*delay_ms),
                        _ => None,
                    },
                )
            {
                return delay_ms;
            }
            self.changed.notified().await;
        }
    }

    /// Returns a defensive snapshot for terminal ordering assertions.
    fn snapshot(&self) -> Vec<AgentEvent> {
        self.events.lock().expect("event lock").clone()
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    /// Records one event before waking retry lifecycle observers.
    async fn emit(&self, event: AgentEvent) -> Result<(), kernel::SinkError> {
        self.events.lock().expect("event lock").push(event);
        self.changed.notify_waiters();
        Ok(())
    }
}

/// Owns one retry-configured kernel, model, session, and event sink.
struct RetryFixture {
    kernel: Arc<Kernel>,
    model: Arc<ScriptedModel>,
    session_id: SessionId,
    sink: Arc<RecordingSink>,
    _root: tempfile::TempDir,
}

impl RetryFixture {
    /// Builds and registers one deterministic retry session.
    async fn new(
        responses: Vec<ScriptedResponse>,
        retry_policy: RetryPolicy,
    ) -> Self {
        let root = tempfile::tempdir().expect("store root");
        let cwd = root.path().join("workspace");
        std::fs::create_dir_all(&cwd).expect("create project cwd");
        let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(10_000)));
        let model = Arc::new(ScriptedModel {
            responses: Mutex::new(VecDeque::from(responses)),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        });
        let kernel = Arc::new(
            KernelFactory::builder()
                .model_factory(Arc::new(StaticModelFactory(
                    Arc::clone(&model) as Arc<dyn Model>
                )))
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
                .clock(clock)
                .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
                .retry_policy(retry_policy)
                .build()
                .build()
                .expect("build kernel"),
        );
        let session_id =
            SessionId::try_from("session-retry").expect("session id");
        kernel
            .create_session(SessionCreateOptions {
                session_id: session_id.clone(),
                cwd,
                parent_session_id: None,
            })
            .await
            .expect("create session");
        Self {
            kernel,
            model,
            session_id,
            sink: Arc::new(RecordingSink::default()),
            _root: root,
        }
    }

    /// Starts one run while retaining fixture ownership for event inspection.
    fn start(
        &self,
    ) -> tokio::task::JoinHandle<Result<RunResult, kernel::KernelError>> {
        let kernel = Arc::clone(&self.kernel);
        let session_id = self.session_id.clone();
        let sink = Arc::clone(&self.sink);
        tokio::spawn(async move {
            kernel
                .run(
                    RunRequest {
                        session_id,
                        input: "hello".into(),
                    },
                    sink,
                )
                .await
        })
    }
}

/// Pi's non-retryable error classes override a provider's transient hint.
#[test]
fn classifier_rejects_auth_quota_billing_and_context_failures() {
    for summary in [
        "authentication failed",
        "quota exceeded",
        "billing is not active",
        "context window exceeded",
    ] {
        let failure = ModelFailure {
            summary: summary.to_string(),
            status: Some(429),
            retry_disposition: ModelRetryDisposition::Retryable,
        };
        assert_eq!(
            RetryClassifier::classify(&failure),
            ModelRetryDisposition::NonRetryable
        );
    }
}

/// Three transient failures retry after 2/4/8 seconds in distinct Turns of one run.
#[tokio::test(start_paused = true)]
async fn transient_errors_retry_three_times_with_pi_backoff() {
    let failures = ["rate limited", "service unavailable", "terminated"]
        .into_iter()
        .map(|summary| {
            ScriptedResponse::Failure(ModelFailure {
                summary: summary.to_string(),
                status: Some(503),
                retry_disposition: ModelRetryDisposition::Retryable,
            })
        });
    let fixture = RetryFixture::new(
        failures
            .chain([ScriptedModel::text("ok")])
            .collect::<Vec<_>>(),
        RetryPolicy::builder()
            .enabled(true)
            .max_retries(3)
            .base_delay_ms(2_000)
            .build(),
    )
    .await;
    let run = fixture.start();

    for (attempt, delay_ms) in [(1, 2_000), (2, 4_000), (3, 8_000)] {
        assert_eq!(fixture.sink.retry_delay(attempt).await, delay_ms);
        tokio::time::advance(Duration::from_millis(delay_ms)).await;
        tokio::task::yield_now().await;
    }

    let result = run.await.expect("join run").expect("retry run");
    assert_eq!(result.turns.len(), 4);
    assert!(
        result
            .turns
            .iter()
            .all(|turn| turn.identity.run_id == result.run_id)
    );
    let turn_ids = result
        .turns
        .iter()
        .map(|turn| turn.identity.turn_id.clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(turn_ids.len(), 4);
    assert_eq!(fixture.model.calls.load(Ordering::Relaxed), 4);
    assert!(
        fixture
            .model
            .requests
            .lock()
            .expect("request lock")
            .iter()
            .skip(1)
            .all(|request| request.messages.iter().all(|message| {
                !matches!(
                    &message.content,
                    MessageContent::Assistant { metadata, .. }
                        if metadata.error.is_some()
                )
            }))
    );
    let events = fixture.sink.snapshot();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event.payload,
                AgentEventPayload::AgentSettled { .. }
            ))
            .count(),
        1
    );
    let run_end = events
        .iter()
        .position(|event| {
            matches!(event.payload, AgentEventPayload::RunEnd { .. })
        })
        .expect("RunEnd");
    let settled = events
        .iter()
        .position(|event| {
            matches!(event.payload, AgentEventPayload::AgentSettled { .. })
        })
        .expect("AgentSettled");
    assert!(run_end < settled);
}

/// Authentication failures settle immediately even when the provider marks them transient.
#[tokio::test(start_paused = true)]
async fn non_retryable_failure_does_not_consume_retry_budget() {
    let fixture = RetryFixture::new(
        vec![ScriptedResponse::Failure(ModelFailure {
            summary: "authentication failed".to_string(),
            status: Some(429),
            retry_disposition: ModelRetryDisposition::Retryable,
        })],
        RetryPolicy::builder()
            .enabled(true)
            .max_retries(3)
            .base_delay_ms(2_000)
            .build(),
    )
    .await;

    let result = fixture
        .start()
        .await
        .expect("join run")
        .expect("failed run settles");
    assert_eq!(result.turns.len(), 1);
    assert!(matches!(
        result.turns[0].outcome,
        protocol::TurnOutcome::Failed { .. }
    ));
    assert_eq!(fixture.model.calls.load(Ordering::Relaxed), 1);
    assert!(!fixture.sink.snapshot().iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::RetryScheduled { .. }
    )));
}

/// Cancellation interrupts backoff while preserving queued follow-up work.
#[tokio::test(start_paused = true)]
async fn cancellation_during_backoff_settles_and_preserves_follow_up() {
    let fixture = RetryFixture::new(
        vec![
            ScriptedResponse::Failure(ModelFailure {
                summary: "service unavailable".to_string(),
                status: Some(503),
                retry_disposition: ModelRetryDisposition::Retryable,
            }),
            ScriptedModel::text("must not run"),
        ],
        RetryPolicy::builder()
            .enabled(true)
            .max_retries(3)
            .base_delay_ms(2_000)
            .build(),
    )
    .await;
    let run = fixture.start();
    assert_eq!(fixture.sink.retry_delay(1).await, 2_000);
    let queued = fixture
        .kernel
        .queue_message(
            &fixture.session_id,
            QueueKind::FollowUp,
            "later".to_string(),
        )
        .await
        .expect("queue follow-up");
    fixture
        .kernel
        .cancel_session(&fixture.session_id)
        .expect("cancel backoff");

    let result = run.await.expect("join run").expect("cancelled retry run");
    assert_eq!(result.turns.len(), 1);
    assert_eq!(fixture.model.calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        fixture
            .kernel
            .pending_messages(&fixture.session_id)
            .expect("pending")
            .follow_up,
        vec![queued]
    );
    assert_eq!(
        fixture
            .sink
            .snapshot()
            .iter()
            .filter(|event| matches!(
                event.payload,
                AgentEventPayload::AgentSettled { .. }
            ))
            .count(),
        1
    );
}
