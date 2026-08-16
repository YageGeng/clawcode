use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use extension::StaticExtensionFactory;
use futures::{Stream, stream};
use kernel::{
    ContextUsageEstimate, EventSink, Kernel, KernelFactory, Model, ModelError,
    ModelFactory,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, AssistantMetadata,
    CompactionPolicy, ContentBlock, IdGenerator, IdKind, MessageContent,
    MessageId, MessageIdentity, MessageTiming, ModelFailure, ModelFinal,
    ModelProfile, ModelRequest, ModelRetryDisposition, ModelStreamEvent,
    ModelUsage, RetryPolicy, RunRequest, RunResult, SessionId, StopReason,
    TimestampMs, TurnId, TurnOutcome,
};
use store::{Clock, JsonlStoreFactory, SessionCreateOptions};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

type TestModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// Shared deterministic capabilities used by compaction model fixtures.
static TEST_MODEL_PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("compaction".to_string())
        .display_name("Compaction fixture".to_string())
        .context_tokens(100_000)
        .max_output_tokens(8_000)
        .build()
});

/// Type-driven transcript builder for context-usage estimation tests.
#[derive(Default)]
struct UsageHistory {
    messages: Vec<AgentMessage>,
}

impl UsageHistory {
    /// Appends one successful Assistant carrying provider usage.
    fn assistant_usage(mut self, total_tokens: u64) -> Self {
        let index = self.messages.len();
        let timestamp = TimestampMs::from(
            1_000_u64.saturating_add(u64::try_from(index).unwrap_or(u64::MAX)),
        );
        self.messages.push(AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(format!("message-{index}"))
                    .expect("message id"),
                turn_id: TurnId::try_from(format!("turn-{index}"))
                    .expect("turn id"),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .expect("message timing"),
            content: MessageContent::Assistant {
                blocks: vec![ContentBlock::Text {
                    text: "answer".to_string(),
                }],
                metadata: AssistantMetadata::builder()
                    .provider_id("fixture".to_string())
                    .model_id("compaction".to_string())
                    .stop_reason(StopReason::EndTurn)
                    .usage(
                        ModelUsage::builder()
                            .input_tokens(total_tokens)
                            .output_tokens(0)
                            .cache_read_tokens(0)
                            .cache_write_tokens(0)
                            .total_tokens(total_tokens)
                            .build(),
                    )
                    .build(),
            },
        });
        self
    }

    /// Appends one user message whose text has a predictable four-chars-per-token estimate.
    fn user_text(mut self, text: String) -> Self {
        let index = self.messages.len();
        let timestamp = TimestampMs::from(
            1_000_u64.saturating_add(u64::try_from(index).unwrap_or(u64::MAX)),
        );
        self.messages.push(AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(format!("message-{index}"))
                    .expect("message id"),
                turn_id: TurnId::try_from(format!("turn-{index}"))
                    .expect("turn id"),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .expect("message timing"),
            content: MessageContent::User {
                blocks: vec![ContentBlock::Text { text }],
            },
        });
        self
    }
}

/// Latest valid Assistant usage is combined with estimated trailing messages.
#[test]
fn estimate_uses_latest_valid_usage_plus_trailing_messages() {
    let history = UsageHistory::default()
        .assistant_usage(70_000)
        .assistant_usage(40_000)
        .user_text("x".repeat(4_000));
    let estimate = ContextUsageEstimate::from_history(&history.messages);
    assert_eq!(estimate.usage_tokens, 40_000);
    assert_eq!(estimate.trailing_tokens, 1_000);
    assert_eq!(estimate.tokens, 41_000);
}

/// Supplies deterministic increasing milliseconds to compaction timing assertions.
struct StepClock(AtomicU64);

impl Clock for StepClock {
    /// Advances by one millisecond for each observation.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// Supplies stable readable ids for operation, Turn, message, and entry records.
struct SequentialIds(AtomicU64);

impl IdGenerator for SequentialIds {
    /// Generates one unique identifier using the domain prefix.
    fn next(&self, kind: IdKind) -> String {
        let value = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        format!("{}-{value}", kind.prefix())
    }
}

/// Streams scripted responses or fails before a stream is created.
struct ScriptedModel {
    responses: Mutex<VecDeque<Result<Vec<ModelStreamEvent>, ModelError>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    /// Builds a successful terminal event with explicit test usage.
    fn finished(stop_reason: StopReason) -> ModelStreamEvent {
        ModelStreamEvent::Finished(ModelFinal {
            stop_reason,
            raw_stop_reason: None,
            usage: ModelUsage::builder()
                .input_tokens(10)
                .output_tokens(5)
                .cache_read_tokens(0)
                .cache_write_tokens(0)
                .total_tokens(15)
                .build(),
        })
    }

    /// Builds the explicit provider failure used to exercise overflow recovery.
    fn context_overflow() -> ModelError {
        ModelError::Request(ModelFailure {
            summary: "Your input exceeds the context window of this model"
                .to_string(),
            status: Some(400),
            retry_disposition: ModelRetryDisposition::NonRetryable,
        })
    }
}

#[async_trait]
impl Model for ScriptedModel {
    /// Returns the immutable profile used by compaction requests.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that the scripted compaction model is locally ready.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Records each request and returns the next scripted outcome.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        self.requests.lock().expect("request lock").push(request);
        let events = self
            .responses
            .lock()
            .expect("response lock")
            .pop_front()
            .ok_or_else(|| {
                ModelError::Request(ModelFailure {
                    summary: "missing response".to_string(),
                    status: None,
                    retry_disposition: ModelRetryDisposition::NonRetryable,
                })
            })??;
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

/// Returns the shared deterministic model from the factory boundary.
struct StaticModelFactory(Arc<dyn Model>);

impl ModelFactory for StaticModelFactory {
    /// Clones the configured model without provider access.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0))
    }
}

/// Records compaction events in delivery order.
#[derive(Default)]
struct RecordingSink(Mutex<Vec<AgentEvent>>);

impl RecordingSink {
    /// Returns a defensive event snapshot for assertions.
    fn events(&self) -> Vec<AgentEvent> {
        self.0.lock().expect("sink lock").clone()
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    /// Retains every emitted event without external I/O.
    async fn emit(&self, event: AgentEvent) -> Result<(), kernel::SinkError> {
        self.0.lock().expect("sink lock").push(event);
        Ok(())
    }
}

/// Owns one session with history prepared before compaction starts.
#[derive(typed_builder::TypedBuilder)]
struct CompactionFixture {
    kernel: Arc<Kernel>,
    session_id: SessionId,
    messages_before: Vec<AgentMessage>,
    _store_root: tempfile::TempDir,
}

impl CompactionFixture {
    /// Creates history followed by a successful model-generated summary.
    async fn with_history_and_summary(summary: &str) -> Self {
        Self::new([Ok(vec![
            ModelStreamEvent::TextDelta(summary.to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ])])
        .await
    }

    /// Creates history followed by a model request failure during compaction.
    async fn with_failing_model() -> Self {
        Self::new([Err(ModelError::Request(ModelFailure {
            summary: "summary failed".to_string(),
            status: None,
            retry_disposition: ModelRetryDisposition::NonRetryable,
        }))])
        .await
    }

    /// Builds a deterministic kernel with summary attempts and one persisted historical Turn.
    async fn new(
        compaction_responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
    ) -> Self {
        let root = tempfile::tempdir().expect("store root");
        let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(50_000)));
        let mut responses = VecDeque::from([Ok(vec![
            ModelStreamEvent::TextDelta("historical answer".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ])]);
        responses.extend(compaction_responses);
        let model: Arc<dyn Model> = Arc::new(ScriptedModel {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        });
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
                .clock(clock)
                .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
                .retry_policy(
                    RetryPolicy::builder()
                        .enabled(true)
                        .max_retries(3)
                        .base_delay_ms(0)
                        .build(),
                )
                .compaction_policy(
                    CompactionPolicy::builder()
                        .enabled(true)
                        .reserve_tokens(16_384)
                        .keep_recent_tokens(0)
                        .build(),
                )
                .build()
                .build()
                .expect("build kernel"),
        );
        let session_id =
            SessionId::try_from("session-compact").expect("session id");
        let cwd = root.path().join("workspace/compact");
        std::fs::create_dir_all(&cwd).expect("create project cwd");
        kernel
            .create_session(SessionCreateOptions {
                session_id: session_id.clone(),
                cwd,
                parent_session_id: None,
            })
            .await
            .expect("create session");
        kernel
            .run(
                RunRequest {
                    session_id: session_id.clone(),
                    input: "historical question".to_string(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
            .expect("prepare history");
        let messages_before =
            kernel.session_messages(&session_id).expect("history");
        Self::builder()
            .kernel(kernel)
            .session_id(session_id)
            .messages_before(messages_before)
            ._store_root(root)
            .build()
    }
}

/// Successful compaction streams a summary and persists exact pi v4 fields.
#[tokio::test]
async fn compact_streams_summary_and_persists_pi_v4_payload() {
    let fixture =
        CompactionFixture::with_history_and_summary("summary text").await;
    let sink = Arc::new(RecordingSink::default());

    let result = fixture
        .kernel
        .compact_session(&fixture.session_id, sink.clone())
        .await
        .expect("compact");
    let tree = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("tree");
    let entry = tree
        .entries
        .iter()
        .find(|entry| entry.entry_id == result.entry_id)
        .expect("entry");
    assert_eq!(entry.payload["summary"], "summary text");
    assert!(entry.payload["retainedTail"].is_array());
    assert!(entry.payload["tokensBefore"].is_u64());
    assert_eq!(entry.payload["details"]["turnId"], result.turn_id.as_str());
    assert_eq!(entry.payload["details"]["reason"], "manual");
    assert!(sink.events().iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionEnd { .. }
    )));
    assert_eq!(
        fixture
            .kernel
            .session_transcript(&fixture.session_id)
            .expect("full transcript"),
        fixture.messages_before,
        "protocol replay must retain messages hidden from active model context"
    );
}

/// A transient summary failure uses the configured retry budget without moving the leaf early.
#[tokio::test]
async fn compaction_summary_retries_transient_model_failure() {
    let fixture = CompactionFixture::new([
        Err(ModelError::Stream(ModelFailure {
            summary: "connection terminated".to_string(),
            status: None,
            retry_disposition: ModelRetryDisposition::Retryable,
        })),
        Ok(vec![
            ModelStreamEvent::TextDelta("summary after retry".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ]),
    ])
    .await;

    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("retried compaction");
    assert_eq!(
        fixture
            .kernel
            .session_tree(&fixture.session_id)
            .expect("tree")
            .entries
            .iter()
            .filter(|entry| entry.kind == "compaction")
            .count(),
        1
    );
}

/// A summary failure leaves both the active leaf and model context unchanged.
#[tokio::test]
async fn failed_summary_does_not_move_leaf_or_replace_history() {
    let fixture = CompactionFixture::with_failing_model().await;
    let before = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("before");

    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect_err("summary failure");
    let after = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("after");
    assert_eq!(after.leaf_id, before.leaf_id);
    assert_eq!(
        fixture
            .kernel
            .session_messages(&fixture.session_id)
            .expect("messages"),
        fixture.messages_before
    );
}

/// Threshold usage compacts after a successful response without repeating that response.
#[tokio::test]
async fn threshold_usage_compacts_without_repeating_assistant_call() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace/threshold");
    std::fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(60_000)));
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(vec![
                ModelStreamEvent::TextDelta("large answer".to_string()),
                ModelStreamEvent::Finished(ModelFinal {
                    stop_reason: StopReason::EndTurn,
                    raw_stop_reason: None,
                    usage: ModelUsage::builder()
                        .input_tokens(83_000)
                        .output_tokens(1_000)
                        .cache_read_tokens(0)
                        .cache_write_tokens(0)
                        .total_tokens(84_000)
                        .build(),
                }),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("threshold summary".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let kernel = KernelFactory::builder()
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
        .compaction_policy(
            CompactionPolicy::builder()
                .enabled(true)
                .reserve_tokens(16_384)
                .keep_recent_tokens(0)
                .build(),
        )
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-threshold").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let sink = Arc::new(RecordingSink::default());

    let result = kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "large context".to_string(),
            },
            sink.clone(),
        )
        .await
        .expect("threshold run");
    assert_eq!(result.turns.len(), 1);
    assert_eq!(
        model.requests.lock().expect("request lock").len(),
        2,
        "one Assistant request and one summary request"
    );
    assert!(sink.events().iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionEnd {
            reason: protocol::CompactionReason::Threshold,
            ..
        }
    )));
    assert!(
        kernel
            .session_tree(&session_id)
            .expect("tree")
            .entries
            .iter()
            .any(|entry| entry.kind == "compaction")
    );
}

/// Owns one deterministic overflow run and exposes provider requests for context assertions.
#[derive(typed_builder::TypedBuilder)]
struct OverflowFixture {
    kernel: Kernel,
    model: Arc<ScriptedModel>,
    session_id: SessionId,
    sink: Arc<RecordingSink>,
    _store_root: tempfile::TempDir,
}

impl OverflowFixture {
    /// Creates one session whose model consumes the supplied attempt and summary sequence.
    async fn new(
        responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
    ) -> Self {
        let root = tempfile::tempdir().expect("store root");
        let cwd = root.path().join("workspace/overflow");
        std::fs::create_dir_all(&cwd).expect("create project cwd");
        let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(70_000)));
        let model = Arc::new(ScriptedModel {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        });
        let kernel = KernelFactory::builder()
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
            .retry_policy(
                RetryPolicy::builder()
                    .enabled(true)
                    .max_retries(3)
                    .base_delay_ms(0)
                    .build(),
            )
            .compaction_policy(
                CompactionPolicy::builder()
                    .enabled(true)
                    .reserve_tokens(16_384)
                    .keep_recent_tokens(0)
                    .build(),
            )
            .build()
            .build()
            .expect("build kernel");
        let session_id =
            SessionId::try_from("session-overflow").expect("session id");
        kernel
            .create_session(SessionCreateOptions {
                session_id: session_id.clone(),
                cwd,
                parent_session_id: None,
            })
            .await
            .expect("create session");
        Self::builder()
            .kernel(kernel)
            .model(model)
            .session_id(session_id)
            .sink(Arc::new(RecordingSink::default()))
            ._store_root(root)
            .build()
    }

    /// Runs the single user prompt used by overflow recovery tests.
    async fn run(&self) -> RunResult {
        self.kernel
            .run(
                RunRequest {
                    session_id: self.session_id.clone(),
                    input: "overflow request".to_string(),
                },
                self.sink.clone(),
            )
            .await
            .expect("overflow run")
    }
}

/// An explicit overflow is compacted once and resumed in a new Turn of the same Run.
#[tokio::test]
async fn explicit_overflow_compacts_once_and_recovers_without_failed_context() {
    let fixture = OverflowFixture::new([
        Err(ScriptedModel::context_overflow()),
        Ok(vec![
            ModelStreamEvent::TextDelta("compact summary".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ]),
        Ok(vec![
            ModelStreamEvent::TextDelta("recovered answer".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ]),
    ])
    .await;

    let result = fixture.run().await;
    let events = fixture.sink.events();
    assert_eq!(result.turns.len(), 2);
    assert_eq!(result.turns[0].identity.run_id, result.run_id);
    assert_eq!(result.turns[1].identity.run_id, result.run_id);
    assert_ne!(
        result.turns[0].identity.turn_id,
        result.turns[1].identity.turn_id
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event.payload,
                AgentEventPayload::CompactionEnd {
                    reason: protocol::CompactionReason::Overflow,
                    ..
                }
            ))
            .count(),
        1
    );
    let requests = fixture.model.requests.lock().expect("request lock");
    assert_eq!(requests.len(), 3);
    assert!(requests[2].messages.iter().all(|message| !matches!(
        &message.content,
        MessageContent::Assistant { metadata, .. }
            if metadata.error.is_some()
    )));
}

/// A second overflow in the resumed Turn settles as failure without another summary call.
#[tokio::test]
async fn second_overflow_does_not_compact_or_retry_again() {
    let fixture = OverflowFixture::new([
        Err(ScriptedModel::context_overflow()),
        Ok(vec![
            ModelStreamEvent::TextDelta("compact summary".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ]),
        Err(ScriptedModel::context_overflow()),
    ])
    .await;

    let result = fixture.run().await;
    assert_eq!(result.turns.len(), 2);
    assert!(matches!(
        result.turns[1].outcome,
        TurnOutcome::Failed { .. }
    ));
    assert_eq!(
        fixture
            .sink
            .events()
            .iter()
            .filter(|event| matches!(
                event.payload,
                AgentEventPayload::CompactionStart {
                    reason: protocol::CompactionReason::Overflow,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        fixture.model.requests.lock().expect("request lock").len(),
        3
    );
}

/// A resumed session compacts stale high usage before persisting and sending the next prompt.
#[tokio::test]
async fn new_prompt_compacts_previous_threshold_before_model_request() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace/pre-prompt");
    std::fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(80_000)));
    let session_id =
        SessionId::try_from("session-pre-prompt").expect("session id");
    let first_model: Arc<dyn Model> = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([Ok(vec![
            ModelStreamEvent::TextDelta("large answer".to_string()),
            ModelStreamEvent::Finished(ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: None,
                usage: ModelUsage::builder()
                    .input_tokens(83_000)
                    .output_tokens(1_000)
                    .cache_read_tokens(0)
                    .cache_write_tokens(0)
                    .total_tokens(84_000)
                    .build(),
            }),
        ])])),
        requests: Mutex::new(Vec::new()),
    });
    let first_kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(first_model)))
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
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(100))))
        .compaction_policy(
            CompactionPolicy::builder()
                .enabled(false)
                .reserve_tokens(16_384)
                .keep_recent_tokens(0)
                .build(),
        )
        .build()
        .build()
        .expect("build first kernel");
    first_kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    first_kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "first prompt".to_string(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("first run");
    drop(first_kernel);

    let resumed_model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(vec![
                ModelStreamEvent::TextDelta("pre-prompt summary".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("second answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let resumed_kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(
            Arc::clone(&resumed_model) as Arc<dyn Model>
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
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(200))))
        .compaction_policy(
            CompactionPolicy::builder()
                .enabled(true)
                .reserve_tokens(16_384)
                .keep_recent_tokens(0)
                .build(),
        )
        .build()
        .build()
        .expect("build resumed kernel");
    resumed_kernel
        .resume_session(session_id.clone(), cwd)
        .await
        .expect("resume session");
    let sink = Arc::new(RecordingSink::default());

    resumed_kernel
        .run(
            RunRequest {
                session_id,
                input: "second prompt".to_string(),
            },
            sink.clone(),
        )
        .await
        .expect("second run");
    assert_eq!(
        resumed_model.requests.lock().expect("request lock").len(),
        2,
        "summary must run before the second Assistant request"
    );
    let compaction_index = sink
        .events()
        .iter()
        .position(|event| {
            matches!(
                event.payload,
                AgentEventPayload::CompactionEnd {
                    reason: protocol::CompactionReason::Threshold,
                    ..
                }
            )
        })
        .expect("threshold compaction event");
    let user_index = sink
        .events()
        .iter()
        .position(|event| {
            matches!(
                &event.payload,
                AgentEventPayload::MessageEnd { message }
                    if matches!(message.content, MessageContent::User { .. })
            )
        })
        .expect("user message event");
    assert!(compaction_index < user_index);
}
