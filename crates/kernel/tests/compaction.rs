use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionModule,
    ExtensionRegistrar, SessionBeforeCompactPoint, SessionBeforeTreePoint,
    SessionCompactFailedPoint, StaticExtensionFactory,
};
use futures::{Stream, stream};
use kernel::{
    ContextUsageEstimate, EventSink, Kernel, KernelFactory, Model, ModelError,
    ModelFactory,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, AssistantMetadata,
    CompactionPolicy, ContentBlock, IdGenerator, IdKind, LaneId,
    MessageContent, MessageId, MessageIdentity, MessageTiming, ModelFailure,
    ModelFinal, ModelProfile, ModelRequest, ModelRetryDisposition,
    ModelStreamEvent, ModelUsage, RetryPolicy, RunRequest, RunResult,
    SessionId, SessionReplayItem, StopReason, TimestampMs, ToolCall,
    ToolCallId, TurnId, TurnOutcome,
};
use store::{
    Clock, EntryKind, JsonlStoreFactory, NewEntry, NewRecord,
    SessionCreateOptions, SessionEntry, SessionForkOptions, SessionMetadata,
    SessionRecord, SessionStore, StoreError, StoreFactory,
};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

#[path = "support/spans.rs"]
mod spans;
mod support;

use support::CapturedLogs;

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
                model_message: None,
                blocks: vec![ContentBlock::Text {
                    text: "answer".to_string(),
                }],
                metadata: AssistantMetadata::builder()
                    .provider_id("fixture".to_string())
                    .model_id("compaction".to_string())
                    .stop_reason(StopReason::EndTurn)
                    .usage(protocol::ModelUsage::from(
                        protocol::Usage::new()
                            .input_tokens(total_tokens)
                            .output_tokens(0)
                            .cached_input_tokens(0)
                            .cache_creation_input_tokens(0)
                            .total_tokens(total_tokens),
                    ))
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
        ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(stop_reason)
                .raw_stop_reason(None)
                .usage(protocol::ModelUsage::from(
                    protocol::Usage::new()
                        .input_tokens(10)
                        .output_tokens(5)
                        .cached_input_tokens(0)
                        .cache_creation_input_tokens(0)
                        .total_tokens(15),
                ))
                .build(),
        )
    }

    /// Builds the explicit provider failure used to exercise overflow recovery.
    fn context_overflow() -> ModelError {
        ModelError::Request(
            ModelFailure::builder()
                .summary(
                    "Your input exceeds the context window of this model"
                        .to_string(),
                )
                .status(Some(400))
                .context_overflow(true)
                .retry_disposition(ModelRetryDisposition::NonRetryable)
                .build(),
        )
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
                ModelError::Request(
                    ModelFailure::builder()
                        .summary("missing response".to_string())
                        .status(None)
                        .retry_disposition(ModelRetryDisposition::NonRetryable)
                        .build(),
                )
            })??;
        // A probe reply logs one line inside the caller's span, named by its
        // text; other replies stay unlogged because summaries must not be logged.
        if let Some(probe) = events.iter().find_map(|event| match event {
            ModelStreamEvent::TextDelta(text)
                if text.starts_with("span-probe-") =>
            {
                Some(text.as_str())
            }
            _ => None,
        }) {
            tracing::info!("scripted model streaming {}", probe);
        }
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

/// Wraps the real JSONL factory with one selectively armed compaction-entry failure.
struct FailingCompactionStoreFactory {
    inner: JsonlStoreFactory,
    fail_next_compaction: Arc<AtomicBool>,
}

impl StoreFactory for FailingCompactionStoreFactory {
    /// Creates a real session whose compaction-entry append can fail once.
    fn create(
        &self,
        options: SessionCreateOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(FailingCompactionStore {
            inner: self.inner.create(options)?,
            fail_next_compaction: Arc::clone(&self.fail_next_compaction),
        }))
    }

    /// Opens a real session whose compaction-entry append can fail once.
    fn open(&self, path: &Path) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(FailingCompactionStore {
            inner: self.inner.open(path)?,
            fail_next_compaction: Arc::clone(&self.fail_next_compaction),
        }))
    }

    /// Forks a real session whose compaction-entry append can fail once.
    fn fork(
        &self,
        source_path: &Path,
        options: SessionForkOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(FailingCompactionStore {
            inner: self.inner.fork(source_path, options)?,
            fail_next_compaction: Arc::clone(&self.fail_next_compaction),
        }))
    }

    /// Delegates session discovery to the real JSONL factory.
    fn list(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<SessionMetadata>, StoreError> {
        self.inner.list(cwd)
    }

    /// Delegates session deletion to the real JSONL factory.
    fn delete(&self, session_id: &SessionId) -> Result<(), StoreError> {
        self.inner.delete(session_id)
    }
}

/// Delegates all persistence except one armed compaction-entry append.
struct FailingCompactionStore {
    inner: Box<dyn SessionStore>,
    fail_next_compaction: Arc<AtomicBool>,
}

impl SessionStore for FailingCompactionStore {
    /// Returns the delegated session identifier.
    fn session_id(&self) -> &SessionId {
        self.inner.session_id()
    }

    /// Returns the delegated JSONL path.
    fn path(&self) -> &Path {
        self.inner.path()
    }

    /// Delegates lane creation.
    fn create_lane(
        &mut self,
        lane: LaneId,
        at: Option<protocol::EntryId>,
    ) -> Result<(), StoreError> {
        self.inner.create_lane(lane, at)
    }

    /// Delegates lane movement.
    fn move_lane(
        &mut self,
        lane: &LaneId,
        to: Option<protocol::EntryId>,
    ) -> Result<(), StoreError> {
        self.inner.move_lane(lane, to)
    }

    /// Fails one armed compaction append while preserving every other entry.
    fn append_entry(
        &mut self,
        lane: &LaneId,
        entry: NewEntry,
    ) -> Result<SessionEntry, StoreError> {
        if entry.kind == EntryKind::Compaction
            && self.fail_next_compaction.swap(false, Ordering::Relaxed)
        {
            return Err(StoreError::InvalidSession(
                "forced compaction append failure".to_string(),
            ));
        }
        self.inner.append_entry(lane, entry)
    }

    /// Returns the delegated lane leaf.
    fn lane(&self, lane: &LaneId) -> Option<&protocol::EntryId> {
        self.inner.lane(lane)
    }

    /// Returns one delegated entry.
    fn get_entry(&self, id: &protocol::EntryId) -> Option<&SessionEntry> {
        self.inner.get_entry(id)
    }

    /// Returns all delegated entries.
    fn entries(&self) -> Vec<SessionEntry> {
        self.inner.entries()
    }

    /// Returns the delegated branch.
    fn branch(
        &self,
        leaf: &protocol::EntryId,
    ) -> Result<Vec<SessionEntry>, StoreError> {
        self.inner.branch(leaf)
    }

    /// Delegates operation-record persistence so terminal recovery remains observable.
    fn append_record(
        &mut self,
        record: NewRecord,
    ) -> Result<SessionRecord, StoreError> {
        self.inner.append_record(record)
    }

    /// Delegates durability checkpoints to the real store.
    fn sync(&mut self) -> Result<(), StoreError> {
        self.inner.sync()
    }

    /// Returns all delegated operation records.
    fn records(&self) -> Vec<SessionRecord> {
        self.inner.records()
    }

    /// Returns the delegated shared mutation sequence.
    fn last_sequence(&self) -> u64 {
        self.inner.last_sequence()
    }

    /// Delegates session-name persistence.
    fn set_name(&mut self, name: Option<String>) -> Result<(), StoreError> {
        self.inner.set_name(name)
    }

    /// Returns the delegated session name.
    fn name(&self) -> Option<&str> {
        self.inner.name()
    }

    fn was_named(&self) -> bool {
        self.inner.was_named()
    }

    /// Delegates entry-label persistence.
    fn set_label(
        &mut self,
        target_id: protocol::EntryId,
        label: Option<String>,
    ) -> Result<(), StoreError> {
        self.inner.set_label(target_id, label)
    }

    /// Returns the delegated entry label.
    fn label(&self, target_id: &protocol::EntryId) -> Option<&str> {
        self.inner.label(target_id)
    }
}

/// Owns one session with history prepared before compaction starts.
#[derive(typed_builder::TypedBuilder)]
struct CompactionFixture {
    kernel: Arc<Kernel>,
    model: Arc<ScriptedModel>,
    session_id: SessionId,
    messages_before: Vec<AgentMessage>,
    session_path: PathBuf,
    _store_root: tempfile::TempDir,
}

impl CompactionFixture {
    /// Creates history followed by a successful model-generated summary.
    async fn with_history_and_summary(summary: &str) -> Self {
        Self::new(
            [Ok(vec![
                ModelStreamEvent::TextDelta(summary.to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ])],
            true,
        )
        .await
    }

    /// Creates history followed by a model request failure during compaction.
    async fn with_failing_model() -> Self {
        Self::new(
            [Err(ModelError::Request(
                ModelFailure::builder()
                    .summary("summary failed".to_string())
                    .status(None)
                    .retry_disposition(ModelRetryDisposition::NonRetryable)
                    .build(),
            ))],
            true,
        )
        .await
    }

    /// Builds a deterministic kernel with summary attempts and one persisted historical Turn.
    async fn new(
        compaction_responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
        prepare_history: bool,
    ) -> Self {
        // A lone Turn is always split because its last Assistant is kept as
        // the newest cut point. Two Turns with a budget equal to the second
        // Turn's estimate (4 + 4 tokens) cut on its user message, so the first
        // Turn is summarized whole.
        Self::with_history_prompts(
            &["historical question", "recent question"],
            [
                Ok(vec![
                    ModelStreamEvent::TextDelta(
                        "historical answer".to_string(),
                    ),
                    ScriptedModel::finished(StopReason::EndTurn),
                ]),
                Ok(vec![
                    ModelStreamEvent::TextDelta("recent answer".to_string()),
                    ScriptedModel::finished(StopReason::EndTurn),
                ]),
            ],
            compaction_responses,
            prepare_history,
            8,
        )
        .await
    }

    /// Builds the fixture with an explicit recent-tail budget for split-Turn coverage.
    async fn new_with_keep_recent(
        compaction_responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
        prepare_history: bool,
        keep_recent_tokens: u64,
    ) -> Self {
        Self::new_with_scripted_history(
            [Ok(vec![
                ModelStreamEvent::TextDelta("historical answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ])],
            compaction_responses,
            prepare_history,
            keep_recent_tokens,
        )
        .await
    }

    /// Builds the fixture with explicit historical Turns before summary responses.
    async fn new_with_scripted_history(
        history_responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
        compaction_responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
        prepare_history: bool,
        keep_recent_tokens: u64,
    ) -> Self {
        Self::with_history_prompts(
            &["historical question"],
            history_responses,
            compaction_responses,
            prepare_history,
            keep_recent_tokens,
        )
        .await
    }

    /// Builds the fixture by running each history prompt in order before summary responses.
    async fn with_history_prompts(
        history_prompts: &[&str],
        history_responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
        compaction_responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
        prepare_history: bool,
        keep_recent_tokens: u64,
    ) -> Self {
        let root = tempfile::tempdir().expect("store root");
        let cwd = root.path().join("workspace/compact");
        std::fs::create_dir_all(&cwd).expect("create project cwd");
        std::fs::write(cwd.join("input.txt"), "input")
            .expect("write input fixture");
        let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(50_000)));
        let mut responses =
            history_responses.into_iter().collect::<VecDeque<_>>();
        responses.extend(compaction_responses);
        let model = Arc::new(ScriptedModel {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        });
        let store_factory =
            Arc::new(JsonlStoreFactory::new(root.path(), Arc::clone(&clock)));
        let kernel = Arc::new(
            KernelFactory::builder()
                .model_factory(Arc::new(StaticModelFactory(
                    Arc::clone(&model) as Arc<dyn Model>
                )))
                .tool_factory(Arc::new(BuiltinToolFactory::new()))
                .store_factory(
                    Arc::clone(&store_factory) as Arc<dyn StoreFactory>
                )
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
                        .max_retry_delay_ms(60_000)
                        .build(),
                )
                .compaction_policy(
                    CompactionPolicy::builder()
                        .enabled(true)
                        .reserve_tokens(16_384)
                        .keep_recent_tokens(keep_recent_tokens)
                        .build(),
                )
                .build()
                .build()
                .expect("build kernel"),
        );
        let session_id =
            SessionId::try_from("session-compact").expect("session id");
        kernel
            .create_session(SessionCreateOptions {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
                parent_session_id: None,
            })
            .await
            .expect("create session");
        if prepare_history {
            for prompt in history_prompts {
                kernel
                    .run(
                        RunRequest {
                            session_id: session_id.clone(),
                            input: (*prompt).into(),
                        },
                        Arc::new(RecordingSink::default()),
                    )
                    .await
                    .expect("prepare history");
            }
        }
        let messages_before =
            kernel.session_messages(&session_id).expect("history");
        let session_path = store_factory
            .list(Some(&cwd))
            .expect("list sessions")
            .into_iter()
            .find(|metadata| metadata.id == session_id)
            .expect("session metadata")
            .path;
        Self::builder()
            .kernel(kernel)
            .model(model)
            .session_id(session_id)
            .messages_before(messages_before)
            .session_path(session_path)
            ._store_root(root)
            .build()
    }
}

/// A Session containing only direct Slash Command records has no model context to compact.
#[tokio::test]
async fn compact_rejects_session_without_model_context() {
    let fixture = CompactionFixture::new([], false).await;
    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/name command-only".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("rename command");
    let before = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("tree before compaction");

    let error = fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect_err("command-only history must not be compacted");
    let after = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("tree after compaction");

    assert_eq!(error.to_string(), "there is no context to compact");
    assert_eq!(after.leaf_id, before.leaf_id);
    assert_eq!(
        after
            .entries
            .iter()
            .filter(|entry| entry.kind == "compaction")
            .count(),
        0
    );
    assert!(
        fixture
            .model
            .requests
            .lock()
            .expect("request lock")
            .is_empty(),
        "Slash Command-only compaction must not reach the provider"
    );
}

/// Manual compaction sends a dedicated pi-style summary protocol instead of raw history roles.
#[tokio::test]
async fn compact_builds_structured_summary_request_with_additional_focus() {
    let fixture =
        CompactionFixture::with_history_and_summary("summary text").await;

    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/compact preserve database schema".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact command");

    let requests = fixture.model.requests.lock().expect("request lock");
    // Two historical Turns precede the summary request.
    let request = requests.get(2).expect("summary request");
    assert!(request.tools.is_empty());
    assert_eq!(request.options.max_tokens, Some(8_000));
    // Summaries reason at the Session's current level, as in pi.
    assert_eq!(
        request.options.thinking_level,
        fixture
            .kernel
            .session_settings(&fixture.session_id)
            .expect("settings")
            .thinking_level
    );
    assert_eq!(request.messages.len(), 2);
    let MessageContent::System { blocks } = &request.messages[0].content else {
        panic!("summary protocol must use a dedicated System message");
    };
    assert_eq!(
        blocks[0].text(),
        Some(
            "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary."
        )
    );
    let MessageContent::User { blocks } = &request.messages[1].content else {
        panic!("serialized conversation must be sent as one User message");
    };
    let prompt = blocks[0].text().expect("summary prompt text");
    assert!(prompt.starts_with("<conversation>\n"));
    assert!(prompt.contains("historical question"));
    assert!(prompt.contains("historical answer"));
    assert!(prompt.contains("\n</conversation>\n\n"));
    assert!(prompt.contains("## Goal"));
    assert!(prompt.contains("## Constraints & Preferences"));
    assert!(prompt.contains("## Progress\n### Done"));
    assert!(prompt.contains("## Key Decisions"));
    assert!(prompt.contains("## Next Steps"));
    assert!(prompt.contains("## Critical Context"));
    assert!(prompt.ends_with("Additional focus: preserve database schema"));
    let tree = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("compaction tree");
    let compaction = tree
        .entries
        .iter()
        .find(|entry| entry.kind == "compaction")
        .expect("compaction entry");
    assert!(
        compaction.payload["retainedTail"]
            .as_array()
            .expect("retained tail")
            .iter()
            .all(|message| message["content"]["type"] != "slash_command"),
        "direct command messages must not enter retained model context"
    );
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
    assert!(entry.payload["tokensBefore"].is_string());
    assert_eq!(entry.payload["usage"]["usage"]["input_tokens"], 10);
    assert_eq!(entry.payload["usage"]["usage"]["output_tokens"], 5);
    assert_eq!(entry.payload["details"]["turnId"], result.turn_id.as_str());
    assert_eq!(entry.payload["details"]["reason"], "manual");
    assert_eq!(entry.payload["details"]["readFiles"], serde_json::json!([]));
    assert_eq!(
        entry.payload["details"]["modifiedFiles"],
        serde_json::json!([])
    );
    assert_eq!(result.summary, "summary text");
    assert_eq!(
        result
            .usage
            .as_ref()
            .expect("summary usage")
            .as_ref()
            .total_tokens,
        Some(15)
    );
    let mutations = std::fs::read_to_string(&fixture.session_path)
        .expect("session JSONL")
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).expect("mutation")
        })
        .collect::<Vec<_>>();
    let operation_started = mutations
        .iter()
        .find(|value| {
            value["type"] == "operation_started"
                && value["intent"]["kind"] == "compaction"
        })
        .expect("compaction operation_started");
    assert!(operation_started.get("sourceLeafId").is_some());
    assert_eq!(
        operation_started["intent"]["resultEntryId"],
        result.entry_id.as_str()
    );
    let attempt = mutations
        .iter()
        .find(|value| {
            value["type"] == "step_attempt" && value["step"] == "compaction"
        })
        .expect("compaction step_attempt");
    assert_eq!(attempt["attempt"], 1);
    assert_eq!(attempt["compactionReason"], "manual");
    let usage = mutations
        .iter()
        .find(|value| {
            value["type"] == "usage" && value["cause"] == "compaction"
        })
        .expect("compaction usage record");
    assert_eq!(usage["entryId"], result.entry_id.as_str());
    assert_eq!(usage["attempt"], 1);
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

/// Summary generation also treats the final native answer as authoritative over deltas.
#[tokio::test]
async fn compaction_uses_corrected_native_summary() {
    let native = rig_core::message::AssistantMessage::new(vec![
        rig_core::message::AssistantContent::text("authoritative summary"),
    ])
    .with_stop(rig_core::message::StopReason::Stop);
    let fixture = CompactionFixture::new(
        [Ok(vec![
            ModelStreamEvent::TextDelta("draft summary".into()),
            ModelStreamEvent::Finished(
                ModelFinal::builder()
                    .stop_reason(StopReason::EndTurn)
                    .usage(ModelUsage::default())
                    .model_message(Some(Box::new(native)))
                    .build(),
            ),
        ])],
        true,
    )
    .await;
    let result = fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact");
    assert_eq!(result.summary, "authoritative summary");
}

/// Compaction lifecycle logs retain operation correlation without summary content.
#[tokio::test(flavor = "current_thread")]
async fn compaction_lifecycle_is_logged_without_summary_content() {
    let fixture = CompactionFixture::with_history_and_summary(
        "sensitive-compaction-summary",
    )
    .await;
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(logs.clone())
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact session");

    let output = logs.content();
    for lifecycle in [
        "started Compaction Turn",
        "completed Compaction Turn",
        "session session-compact",
        "reason Manual",
    ] {
        assert!(
            output.contains(lifecycle),
            "missing {lifecycle} in captured logs: {output:?}"
        );
    }
    // Only the start line names the Session; the Run span carries it onward.
    let completed = output
        .lines()
        .find(|line| line.contains("completed Compaction Turn"))
        .expect("completion line");
    assert!(!completed.contains("session-compact"), "{completed}");
    assert!(!output.contains("sensitive-compaction-summary"));
}

/// Compaction records pi's read/write/edit file metadata and carries it across checkpoints.
#[tokio::test]
async fn compaction_preserves_cumulative_file_operations() {
    // The second Turn is retained, so the first Turn's file operations enter
    // the checkpoint whole instead of through a split-Turn prefix.
    let fixture = CompactionFixture::with_history_prompts(
        &["historical question", "recent question"],
        [
            Ok(vec![
                ModelStreamEvent::ToolCall(
                    ToolCall::builder()
                        .tool_call_id(
                            ToolCallId::try_from("read-call")
                                .expect("read call id"),
                        )
                        .name("read".to_string())
                        .arguments(serde_json::json!({ "path": "input.txt" }))
                        .build(),
                ),
                ModelStreamEvent::ToolCall(
                    ToolCall::builder()
                        .tool_call_id(
                            ToolCallId::try_from("write-call")
                                .expect("write call id"),
                        )
                        .name("write".to_string())
                        .arguments(serde_json::json!({
                            "path": "output.txt",
                            "content": "output"
                        }))
                        .build(),
                ),
                ModelStreamEvent::ToolCall(
                    ToolCall::builder()
                        .tool_call_id(
                            ToolCallId::try_from("edit-call")
                                .expect("edit call id"),
                        )
                        .name("edit".to_string())
                        .arguments(serde_json::json!({
                            "path": "input.txt",
                            "edits": [{
                                "oldText": "input",
                                "newText": "changed"
                            }]
                        }))
                        .build(),
                ),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("historical answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("recent answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ],
        [
            Ok(vec![
                ModelStreamEvent::TextDelta("first summary".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("follow-up answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("updated summary".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ],
        true,
        8,
    )
    .await;

    let first = fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("first compaction");
    assert_eq!(
        first.summary,
        "first summary\n\n<modified-files>\ninput.txt\noutput.txt\n</modified-files>"
    );
    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "follow-up question".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("follow-up run");
    let second = fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("second compaction");
    let tree = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("tree");
    let entry = tree
        .entries
        .iter()
        .find(|entry| entry.entry_id == second.entry_id)
        .expect("second compaction entry");

    assert_eq!(entry.payload["details"]["readFiles"], serde_json::json!([]));
    assert_eq!(
        entry.payload["details"]["modifiedFiles"],
        serde_json::json!(["input.txt", "output.txt"])
    );
    assert_eq!(
        second.summary,
        "updated summary\n\n<modified-files>\ninput.txt\noutput.txt\n</modified-files>"
    );
    let requests = fixture.model.requests.lock().expect("request lock");
    let follow_up = requests.get(4).expect("follow-up provider request");
    assert!(follow_up.messages.iter().any(|message| {
        message.content.text_content().is_some_and(|text| {
            text.contains(
                "<modified-files>\ninput.txt\noutput.txt\n</modified-files>",
            )
        })
    }));
}

/// A retained pre-compaction Assistant must not retrigger its stale overflow signal.
#[tokio::test]
async fn retained_assistant_does_not_repeat_overflow_compaction() {
    let fixture = CompactionFixture::new_with_scripted_history(
        [Ok(vec![
            ModelStreamEvent::TextDelta(
                "silent over-window answer".to_string(),
            ),
            ModelStreamEvent::Finished(
                ModelFinal::builder()
                    .stop_reason(StopReason::EndTurn)
                    .raw_stop_reason(None)
                    .usage(protocol::ModelUsage::from(
                        protocol::Usage::new()
                            .input_tokens(101_000)
                            .output_tokens(10)
                            .cached_input_tokens(0)
                            .cache_creation_input_tokens(0)
                            .total_tokens(101_010),
                    ))
                    .build(),
            ),
        ])],
        [
            Ok(vec![
                ModelStreamEvent::TextDelta("turn prefix summary".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("next answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ],
        true,
        1,
    )
    .await;

    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "next question".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("retained Assistant usage must be stale after compaction");

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
    assert_eq!(
        fixture.model.requests.lock().expect("request lock").len(),
        3
    );
}

/// A summary terminal ToolUse result is invalid even when no ToolCall event preceded it.
#[tokio::test]
async fn compaction_rejects_tool_use_terminal_result() {
    let fixture = CompactionFixture::new(
        [Ok(vec![
            ModelStreamEvent::TextDelta("partial summary".to_string()),
            ScriptedModel::finished(StopReason::ToolUse),
        ])],
        true,
    )
    .await;
    let sink = Arc::new(RecordingSink::default());

    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .await
        .expect_err("ToolUse summary terminal must be rejected");

    assert!(
        fixture
            .kernel
            .session_tree(&fixture.session_id)
            .expect("tree")
            .entries
            .iter()
            .all(|entry| entry.kind != "compaction")
    );
    assert!(sink.events().iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionEnd {
            outcome: protocol::CompactionOutcome::Failed { .. },
            ..
        }
    )));
}

/// Replay preserves durable Turn, Run settlement, and compaction order.
#[tokio::test]
async fn session_replay_includes_persisted_compaction_boundary() {
    let fixture =
        CompactionFixture::with_history_and_summary("summary text").await;
    let result = fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact");

    let replay = fixture
        .kernel
        .session_replay(&fixture.session_id)
        .expect("session replay");

    // Each of the two historical Runs replays a Turn and a RunEnd after its
    // messages, followed by the compaction boundary.
    assert_eq!(replay.len(), fixture.messages_before.len() + 5);
    assert!(matches!(
        replay.get(replay.len().saturating_sub(3)),
        Some(SessionReplayItem::Turn(_))
    ));
    assert!(matches!(
        replay.get(replay.len().saturating_sub(2)),
        Some(SessionReplayItem::RunEnd { .. })
    ));
    assert!(matches!(
        replay.last(),
        Some(SessionReplayItem::Compaction {
            result: replayed,
            ..
        }) if replayed.entry_id == result.entry_id
            && replayed.summary == "summary text"
    ));
}

/// A transient summary failure uses the configured retry budget without moving the leaf early.
#[tokio::test]
async fn compaction_summary_retries_transient_model_failure() {
    let fixture = CompactionFixture::new(
        [
            Err(ModelError::Stream(
                ModelFailure::builder()
                    .summary("connection terminated".to_string())
                    .status(None)
                    .retry_disposition(ModelRetryDisposition::Retryable)
                    .build(),
            )),
            Ok(vec![
                ModelStreamEvent::TextDelta("summary after retry".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ],
        true,
    )
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

/// A summary stopped by the output cap is incomplete and never becomes a checkpoint.
#[tokio::test]
async fn truncated_summary_is_rejected_and_not_persisted() {
    let fixture = CompactionFixture::new(
        [Ok(vec![
            ModelStreamEvent::TextDelta("partial".to_string()),
            ScriptedModel::finished(StopReason::MaxTokens),
        ])],
        true,
    )
    .await;

    let error = fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect_err("truncated summary");

    assert!(
        error.to_string().contains(
            "Summarization failed: generation hit the token cap and the summary is incomplete"
        ),
        "{error}"
    );
    assert!(
        fixture
            .kernel
            .session_tree(&fixture.session_id)
            .expect("tree")
            .entries
            .iter()
            .all(|entry| entry.kind != "compaction")
    );
    assert_eq!(
        fixture
            .kernel
            .session_messages(&fixture.session_id)
            .expect("history"),
        fixture.messages_before
    );
    // Two history answers and exactly one summary attempt: no retry.
    assert_eq!(
        fixture.model.requests.lock().expect("request lock").len(),
        3
    );
}

/// Summary requests use the model output limit and the Session's thinking level.
#[tokio::test]
async fn summary_request_uses_output_limit_and_session_thinking_level() {
    let fixture =
        CompactionFixture::with_history_and_summary("summary text").await;
    // The default level is Off, which would make the assertion vacuous.
    fixture
        .kernel
        .set_session_thinking_level(
            &fixture.session_id,
            protocol::ThinkingLevel::High,
        )
        .await
        .expect("set thinking level");

    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact");

    let requests = fixture.model.requests.lock().expect("request lock");
    let request = requests.last().expect("summary request");
    assert_eq!(request.options.max_tokens, Some(8_000));
    assert_eq!(
        request.options.thinking_level,
        protocol::ThinkingLevel::High
    );
}

/// A summary failure leaves both the active leaf and model context unchanged.
#[tokio::test]
async fn failed_summary_does_not_move_leaf_or_replace_history() {
    let fixture = CompactionFixture::with_failing_model().await;
    let sink = Arc::new(RecordingSink::default());
    let before = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("before");

    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::clone(&sink) as Arc<dyn EventSink>,
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
    assert!(sink.events().iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionEnd {
            outcome: protocol::CompactionOutcome::Failed { .. },
            ..
        }
    )));
}

/// A compaction persistence failure still records and emits one failed terminal outcome.
#[tokio::test]
async fn failed_compaction_append_settles_started_lifecycle() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace/persistence-failure");
    std::fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(55_000)));
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(vec![
                ModelStreamEvent::TextDelta("historical answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("summary".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let fail_next_compaction = Arc::new(AtomicBool::new(false));
    let store_factory = Arc::new(FailingCompactionStoreFactory {
        inner: JsonlStoreFactory::new(root.path(), Arc::clone(&clock)),
        fail_next_compaction: Arc::clone(&fail_next_compaction),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::clone(&store_factory) as Arc<dyn StoreFactory>)
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
        SessionId::try_from("session-persistence-failure").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "historical question".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("prepare history");
    fail_next_compaction.store(true, Ordering::Relaxed);
    let sink = Arc::new(RecordingSink::default());

    kernel
        .compact_session(&session_id, Arc::clone(&sink) as Arc<dyn EventSink>)
        .await
        .expect_err("forced compaction persistence failure");

    assert!(sink.events().iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionEnd {
            outcome: protocol::CompactionOutcome::Failed { .. },
            ..
        }
    )));
    let session_path = store_factory
        .list(Some(&cwd))
        .expect("list sessions")
        .into_iter()
        .find(|metadata| metadata.id == session_id)
        .expect("session metadata")
        .path;
    let has_failed_terminal = std::fs::read_to_string(session_path)
        .expect("session JSONL")
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).expect("mutation")
        })
        .any(|value| {
            value["type"] == "operation_finished"
                && value["outcome"] == "failed"
        });
    assert!(has_failed_terminal);
}

/// A cancelled summary emits a cancelled terminal event without moving the lane leaf.
#[tokio::test]
async fn cancelled_summary_does_not_persist_compaction() {
    let fixture =
        CompactionFixture::new([Err(ModelError::Cancelled)], true).await;
    let sink = Arc::new(RecordingSink::default());
    let before = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("tree before cancellation");

    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .await
        .expect_err("cancelled summary");
    let after = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("tree after cancellation");

    assert_eq!(after.leaf_id, before.leaf_id);
    assert!(after.entries.iter().all(|entry| entry.kind != "compaction"));
    assert!(sink.events().iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionEnd {
            outcome: protocol::CompactionOutcome::Cancelled,
            ..
        }
    )));
}

/// A compaction boundary without newer model-visible messages cannot be compacted again.
#[tokio::test]
async fn compact_rejects_unchanged_compaction_boundary() {
    let fixture = CompactionFixture::new(
        [Ok(vec![
            ModelStreamEvent::TextDelta("first summary".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ])],
        true,
    )
    .await;
    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("first compaction");
    let request_count =
        fixture.model.requests.lock().expect("request lock").len();

    let error = fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect_err("unchanged boundary");

    assert_eq!(error.to_string(), "there is no context to compact");
    assert_eq!(
        fixture.model.requests.lock().expect("request lock").len(),
        request_count
    );
}

/// A second compaction updates the previous summary using only newer conversation messages.
#[tokio::test]
async fn second_compaction_uses_previous_summary_and_new_messages() {
    let fixture = CompactionFixture::new(
        [
            Ok(vec![
                ModelStreamEvent::TextDelta("first summary".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("follow-up answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("updated summary".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ],
        true,
    )
    .await;
    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("first compaction");
    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "follow-up question".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("follow-up run");
    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("second compaction");

    let requests = fixture.model.requests.lock().expect("request lock");
    // Two history answers, the first summary, and the follow-up answer precede it.
    let request = requests.get(4).expect("second summary request");
    let MessageContent::User { blocks } = &request.messages[1].content else {
        panic!("summary prompt must be a User message");
    };
    let prompt = blocks[0].text().expect("summary prompt text");
    assert!(
        prompt
            .contains("<previous-summary>\nfirst summary\n</previous-summary>")
    );
    // Only the Turn retained by the first checkpoint is new to the summary;
    // the follow-up Turn stays verbatim as the recent tail.
    assert!(prompt.contains("recent question"));
    assert!(prompt.contains("recent answer"));
    assert!(!prompt.contains("follow-up question"));
    assert!(!prompt.contains("historical question"));
    assert!(!prompt.contains("historical answer"));
    assert!(prompt.contains("messages above are NEW conversation messages"));
}

/// Retaining an Assistant suffix summarizes the earlier prefix of the same Turn separately.
#[tokio::test]
async fn split_turn_compaction_preserves_prefix_context() {
    let fixture = CompactionFixture::new_with_keep_recent(
        [Ok(vec![
            ModelStreamEvent::TextDelta("turn prefix summary".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ])],
        true,
        1,
    )
    .await;

    let result = fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("split-Turn compaction");

    let requests = fixture.model.requests.lock().expect("request lock");
    let request = requests.get(1).expect("prefix summary request");
    let MessageContent::User { blocks } = &request.messages[1].content else {
        panic!("prefix summary prompt must be a User message");
    };
    let prompt = blocks[0].text().expect("prefix prompt text");
    assert!(prompt.starts_with("# Conversation\n"), "{prompt}");
    assert!(prompt.contains(
        "\n\n# Instructions\nThe messages above are earlier context from an ongoing conversation."
    ));
    assert!(!prompt.contains("This is the PREFIX of a turn"));
    assert!(prompt.contains("historical question"));
    assert_eq!(
        result.summary,
        "No prior history.\n\n---\n\n**Turn Context (split turn):**\n\nturn prefix summary"
    );
    let active = fixture
        .kernel
        .session_messages(&fixture.session_id)
        .expect("active context");
    assert!(matches!(
        active.first().map(|message| &message.content),
        Some(MessageContent::CompactionSummary { .. })
    ));
    assert!(active.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::Assistant { blocks, .. }
                if blocks.iter().any(|block| block.text() == Some("historical answer"))
        )
    }));
}

/// Custom compaction instructions also steer the split-Turn prefix summary.
#[tokio::test]
async fn split_turn_prefix_receives_custom_instructions() {
    let fixture = CompactionFixture::new_with_keep_recent(
        [Ok(vec![
            ModelStreamEvent::TextDelta("turn prefix summary".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ])],
        true,
        1,
    )
    .await;

    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/compact keep the API names".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact command");

    let requests = fixture.model.requests.lock().expect("request lock");
    let request = requests.get(1).expect("prefix summary request");
    let MessageContent::User { blocks } = &request.messages[1].content else {
        panic!("prefix summary prompt must be a User message");
    };
    let prompt = blocks[0].text().expect("prefix prompt text");
    assert!(
        prompt.ends_with("\n\nAdditional focus: keep the API names"),
        "{prompt}"
    );
}

/// Summary input omits reasoning and formats Tool calls as key=value arguments.
#[tokio::test]
async fn summary_input_omits_thinking_and_formats_tool_arguments() {
    // The cut lands on the final Assistant, so the only summary request is
    // the split-Turn prefix covering the reasoning and the Tool call.
    let fixture = CompactionFixture::new_with_scripted_history(
        [
            Ok(vec![
                ModelStreamEvent::ReasoningDelta(
                    "secret reasoning".to_string(),
                ),
                ModelStreamEvent::ToolCall(
                    ToolCall::builder()
                        .tool_call_id(
                            ToolCallId::try_from("read-call")
                                .expect("read call id"),
                        )
                        .name("read".to_string())
                        .arguments(serde_json::json!({ "path": "input.txt" }))
                        .build(),
                ),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("historical answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ],
        [Ok(vec![
            ModelStreamEvent::TextDelta("prefix summary".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ])],
        true,
        1,
    )
    .await;

    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact");

    let requests = fixture.model.requests.lock().expect("request lock");
    assert_eq!(requests.len(), 3, "two history requests and one summary");
    let MessageContent::User { blocks } = &requests[2].messages[1].content
    else {
        panic!("summary prompt must be a User message");
    };
    let prompt = blocks[0].text().expect("summary prompt text");
    assert!(!prompt.contains("secret reasoning"), "{prompt}");
    assert!(!prompt.contains("[Assistant thinking]"), "{prompt}");
    assert!(
        prompt.contains("[Assistant tool calls]: read(path=\"input.txt\")"),
        "{prompt}"
    );
}

/// Recent retention excludes direct command records from the persisted model tail.
#[tokio::test]
async fn retained_tail_excludes_model_invisible_command_messages() {
    let fixture = CompactionFixture::new_with_keep_recent(
        [Ok(vec![
            ModelStreamEvent::TextDelta("turn prefix summary".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ])],
        true,
        1,
    )
    .await;

    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/compact".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact command");
    let tree = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("compaction tree");
    let compaction = tree
        .entries
        .iter()
        .find(|entry| entry.kind == "compaction")
        .expect("compaction entry");

    assert!(
        compaction.payload["retainedTail"]
            .as_array()
            .expect("retained tail")
            .iter()
            .all(|message| message["content"]["type"] != "slash_command"),
        "direct command messages must not enter retained model context"
    );
}

/// One answer whose reported usage, 84,000 tokens, crosses the 83,616-token
/// threshold of `threshold_kernel`.
fn large_usage_answer() -> Vec<ModelStreamEvent> {
    vec![
        ModelStreamEvent::TextDelta("large answer".to_string()),
        ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(StopReason::EndTurn)
                .raw_stop_reason(None)
                .usage(protocol::ModelUsage::from(
                    protocol::Usage::new()
                        .input_tokens(83_000)
                        .output_tokens(1_000)
                        .cached_input_tokens(0)
                        .cache_creation_input_tokens(0)
                        .total_tokens(84_000),
                ))
                .build(),
        ),
    ]
}

/// Builds a Kernel over `model` whose 100k window and 16,384-token reserve
/// compact automatically above 83,616 tokens, with one Session at `session_id`.
async fn threshold_kernel(
    root: &Path,
    model: &Arc<ScriptedModel>,
    keep_recent_tokens: u64,
    session_id: &SessionId,
) -> Kernel {
    threshold_kernel_with(
        root,
        model,
        keep_recent_tokens,
        session_id,
        StaticExtensionFactory::default(),
    )
    .await
}

/// Builds `threshold_kernel` with explicit extensions; the Session cwd holds
/// `input.txt` so scripted `read` calls succeed.
async fn threshold_kernel_with(
    root: &Path,
    model: &Arc<ScriptedModel>,
    keep_recent_tokens: u64,
    session_id: &SessionId,
    extensions: StaticExtensionFactory,
) -> Kernel {
    threshold_kernel_configured(
        root,
        model,
        keep_recent_tokens,
        session_id,
        KernelOptions {
            extensions,
            ..KernelOptions::default()
        },
    )
    .await
}

/// Collaborators a threshold test kernel can replace.
struct KernelOptions {
    extensions: StaticExtensionFactory,
    branch_summary: config::BranchSummaryConfig,
    id_generator: Arc<dyn IdGenerator>,
}

impl Default for KernelOptions {
    /// Uses no extensions, the default branch reserve, and sequential ids.
    fn default() -> Self {
        Self {
            extensions: StaticExtensionFactory::default(),
            branch_summary: config::BranchSummaryConfig::default(),
            id_generator: Arc::new(SequentialIds(AtomicU64::new(0))),
        }
    }
}

/// Builds `threshold_kernel_with` with explicit collaborators.
async fn threshold_kernel_configured(
    root: &Path,
    model: &Arc<ScriptedModel>,
    keep_recent_tokens: u64,
    session_id: &SessionId,
    options: KernelOptions,
) -> Kernel {
    let cwd = root.join("workspace").join(session_id.as_str());
    std::fs::create_dir_all(&cwd).expect("create project cwd");
    std::fs::write(cwd.join("input.txt"), "input").expect("write input");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(60_000)));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(
            Arc::clone(model) as Arc<dyn Model>
        )))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root,
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(options.extensions))
        .branch_summary(options.branch_summary)
        .clock(clock)
        .id_generator(options.id_generator)
        .compaction_policy(
            CompactionPolicy::builder()
                .enabled(true)
                .reserve_tokens(16_384)
                .keep_recent_tokens(keep_recent_tokens)
                .build(),
        )
        .build()
        .build()
        .expect("build kernel");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    kernel
}

/// Threshold usage with nothing compactable skips compaction instead of
/// failing the Run and rejecting every later prompt.
#[tokio::test]
async fn threshold_without_compactable_context_skips_compaction() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(large_usage_answer()),
            Ok(vec![
                ModelStreamEvent::TextDelta("second answer".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-threshold-skip").expect("session id");
    // A recent window larger than the whole history leaves nothing to summarize.
    let kernel =
        threshold_kernel(root.path(), &model, 1_000_000, &session_id).await;

    // The first Run ends over the threshold; the second starts over it.
    for input in ["large context", "next prompt"] {
        let result = kernel
            .run(
                RunRequest {
                    session_id: session_id.clone(),
                    input: input.into(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
            .expect(input);
        assert!(
            matches!(
                result.turns.last().map(|turn| &turn.outcome),
                Some(TurnOutcome::Completed { .. })
            ),
            "{input}"
        );
    }
    assert_eq!(
        model.requests.lock().expect("request lock").len(),
        2,
        "two answers and no summary request"
    );
    assert!(
        !kernel
            .session_tree(&session_id)
            .expect("tree")
            .entries
            .iter()
            .any(|entry| entry.kind == "compaction")
    );
}

/// A post-Run compaction that fails still lets the answered Run settle, so
/// clients see the Run end instead of an open Operation.
#[tokio::test]
async fn failed_post_run_compaction_still_settles_the_run() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(large_usage_answer()),
            Err(ModelError::Request(
                ModelFailure::builder()
                    .summary("summary failed".to_string())
                    .status(None)
                    .retry_disposition(ModelRetryDisposition::NonRetryable)
                    .build(),
            )),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-threshold-failure").expect("session id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;
    let sink = Arc::new(RecordingSink::default());

    let result = kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "large context".into(),
            },
            sink.clone(),
        )
        .await
        .expect("the answered Run succeeds");
    assert_eq!(result.turns.len(), 1);
    assert_eq!(
        model.requests.lock().expect("request lock").len(),
        2,
        "one answer and one failed summary request"
    );
    assert!(sink.events().iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::AgentSettled {
            outcome: protocol::AgentOutcome::Succeeded,
            ..
        }
    )));
}

/// Threshold usage compacts after a successful response without repeating that response.
#[tokio::test]
async fn threshold_usage_compacts_without_repeating_assistant_call() {
    let logs = CapturedLogs::default();
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(large_usage_answer()),
            Ok(vec![
                ModelStreamEvent::TextDelta("threshold summary".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-threshold").expect("session id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;
    let sink = Arc::new(RecordingSink::default());

    let result = kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "large context".into(),
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
    // In-loop compaction nests under the loop's trace with a Run id of its own.
    let start = spans::line_with(
        &logs.content(),
        "started Kernel Run for session session-threshold",
    )
    .expect("log line");
    let (loop_trace, loop_run) = spans::run_fields(&start);
    let summary = spans::line_with(
        &logs.content(),
        "for session session-threshold with reason",
    )
    .expect("log line");
    assert_eq!(summary.matches("run{").count(), 2, "{summary}");
    let (trace, run) = spans::run_fields(&summary);
    assert_eq!(trace, loop_trace, "{summary}");
    assert_ne!(run, loop_run, "{summary}");
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
        Self::with_keep_recent(responses, 0).await
    }

    /// Creates the fixture like `new`, keeping `keep_recent_tokens` out of compaction.
    async fn with_keep_recent(
        responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
        keep_recent_tokens: u64,
    ) -> Self {
        Self::with_extensions(
            responses,
            keep_recent_tokens,
            StaticExtensionFactory::default(),
        )
        .await
    }

    /// Creates the fixture like `with_keep_recent` with explicit extensions.
    async fn with_extensions(
        responses: impl IntoIterator<
            Item = Result<Vec<ModelStreamEvent>, ModelError>,
        >,
        keep_recent_tokens: u64,
        extensions: StaticExtensionFactory,
    ) -> Self {
        let root = tempfile::tempdir().expect("store root");
        let cwd = root.path().join("workspace/overflow");
        std::fs::create_dir_all(&cwd).expect("create project cwd");
        let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(70_000)));
        // One completed historical Turn precedes the overflowing prompt, so
        // recovery has earlier context to compact: a lone user message is
        // always retained as the newest cut point, as in pi.
        let mut scripted = VecDeque::from([Ok(vec![
            ModelStreamEvent::TextDelta("historical answer".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ])]);
        scripted.extend(responses);
        let model = Arc::new(ScriptedModel {
            responses: Mutex::new(scripted),
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
            .extension_factory(Arc::new(extensions))
            .clock(clock)
            .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
            .retry_policy(
                RetryPolicy::builder()
                    .enabled(true)
                    .max_retries(3)
                    .base_delay_ms(0)
                    .max_retry_delay_ms(60_000)
                    .build(),
            )
            .compaction_policy(
                CompactionPolicy::builder()
                    .enabled(true)
                    .reserve_tokens(16_384)
                    .keep_recent_tokens(keep_recent_tokens)
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
        kernel
            .run(
                RunRequest {
                    session_id: session_id.clone(),
                    input: "historical question".into(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
            .expect("prepare overflow history");
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
                    input: "overflow request".into(),
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
    // History answer, overflow, summary, and recovered answer.
    assert_eq!(requests.len(), 4);
    assert!(requests[3].messages.iter().all(|message| !matches!(
        &message.content,
        MessageContent::Assistant { metadata, .. }
            if metadata.error.is_some()
    )));
}

/// An overflow with nothing to compact ends the Run with the provider's failure
/// instead of retrying the same oversized request.
#[tokio::test]
async fn overflow_without_compactable_context_fails_without_retrying() {
    // A recent window larger than the whole history leaves nothing to summarize.
    let fixture = OverflowFixture::with_keep_recent(
        [Err(ScriptedModel::context_overflow())],
        1_000_000,
    )
    .await;

    let result = fixture.run().await;
    assert_eq!(result.turns.len(), 1);
    assert!(matches!(
        result.turns[0].outcome,
        TurnOutcome::Failed { .. }
    ));
    assert_eq!(
        fixture.model.requests.lock().expect("request lock").len(),
        2,
        "the history answer and the overflow, with no retry and no summary request"
    );
}

/// A first prompt that overflows a fresh session has no earlier context to
/// compact, so it fails with the provider error instead of retrying, as in pi.
#[tokio::test]
async fn first_prompt_overflow_fails_without_compaction() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([Err(
            ScriptedModel::context_overflow(),
        )])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-first-overflow").expect("session id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;
    let sink = Arc::new(RecordingSink::default());

    let result = kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "oversized first prompt".into(),
            },
            sink.clone(),
        )
        .await
        .expect("overflow run settles");

    assert_eq!(result.turns.len(), 1);
    assert!(matches!(
        result.turns[0].outcome,
        TurnOutcome::Failed { .. }
    ));
    assert_eq!(
        model.requests.lock().expect("request lock").len(),
        1,
        "no summary request and no retry"
    );
    assert!(!sink.events().iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionStart { .. }
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
    // History answer, overflow, summary, and the second overflow.
    assert_eq!(
        fixture.model.requests.lock().expect("request lock").len(),
        4
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
            ModelStreamEvent::Finished(
                ModelFinal::builder()
                    .stop_reason(StopReason::EndTurn)
                    .raw_stop_reason(None)
                    .usage(protocol::ModelUsage::from(
                        protocol::Usage::new()
                            .input_tokens(83_000)
                            .output_tokens(1_000)
                            .cached_input_tokens(0)
                            .cache_creation_input_tokens(0)
                            .total_tokens(84_000),
                    ))
                    .build(),
            ),
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
                input: "first prompt".into(),
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
                input: "second prompt".into(),
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

/// Returns scripted events for one plain textual answer.
fn plan_answer(text: &str) -> Result<Vec<ModelStreamEvent>, ModelError> {
    Ok(vec![
        ModelStreamEvent::TextDelta(text.to_string()),
        ScriptedModel::finished(StopReason::EndTurn),
    ])
}

/// Returns whether a context message is the kernel's plan-mode note.
fn is_plan_note(message: &AgentMessage) -> bool {
    matches!(
        &message.content,
        MessageContent::Extension { extension }
            if extension.custom_type == "session_mode"
                && extension.extension_id.as_str() == "clawcode"
    )
}

/// Runs one prompt and returns the provider request it produced.
async fn plan_prompt(fixture: &CompactionFixture, input: &str) -> ModelRequest {
    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: input.into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("prompt");
    fixture
        .model
        .requests
        .lock()
        .expect("request lock")
        .last()
        .expect("request")
        .clone()
}

/// Compacting away the note that entered plan mode restates it after the summary,
/// live and after the Session is reopened.
#[tokio::test]
async fn compaction_keeps_the_plan_note() {
    // A 5-token budget keeps the second plan Turn (3 + 4 tokens) but not its
    // Assistant alone, so the cut lands on "plan more" and the note that
    // entered plan mode is summarized away with the first Turn.
    let fixture = CompactionFixture::new_with_scripted_history(
        [
            plan_answer("historical answer"),
            plan_answer("second answer"),
        ],
        [
            plan_answer("summary"),
            plan_answer("after"),
            plan_answer("reopened"),
        ],
        false,
        5,
    )
    .await;
    fixture
        .kernel
        .set_session_mode(&fixture.session_id, protocol::SessionMode::Plan)
        .await
        .expect("set mode");
    plan_prompt(&fixture, "plan this").await;
    plan_prompt(&fixture, "plan more").await;
    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact");

    let live = plan_prompt(&fixture, "after compaction").await;
    assert!(live.messages.get(1).is_some_and(|message| matches!(
        message.content,
        MessageContent::CompactionSummary { .. }
    )));
    // The note is restated after the summary and the retained tail, before
    // the new prompt.
    let note_index = live
        .messages
        .iter()
        .position(is_plan_note)
        .expect("restated plan note");
    assert!(note_index > 1);
    assert!(matches!(
        live.messages
            .get(note_index + 1)
            .map(|message| &message.content),
        Some(MessageContent::User { .. })
    ));

    fixture
        .kernel
        .close_session(&fixture.session_id)
        .await
        .expect("close Session");
    fixture
        .kernel
        .resume_session(
            fixture.session_id.clone(),
            fixture._store_root.path().join("workspace/compact"),
        )
        .await
        .expect("resume Session");
    let rebuilt = plan_prompt(&fixture, "reopened").await;
    assert_eq!(
        live.messages.get(1..=note_index),
        rebuilt.messages.get(1..=note_index)
    );
}

/// Compaction outside plan mode adds no note.
#[tokio::test]
async fn compaction_without_plan_mode_adds_no_note() {
    let fixture = CompactionFixture::new_with_scripted_history(
        [plan_answer("historical answer")],
        [plan_answer("summary"), plan_answer("after")],
        false,
        0,
    )
    .await;
    plan_prompt(&fixture, "plan this").await;
    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact");
    let live = plan_prompt(&fixture, "after compaction").await;
    assert!(!live.messages.iter().any(is_plan_note));
}

/// Standalone compaction is a top-level Run with a trace of its own.
#[tokio::test]
async fn standalone_compaction_opens_a_top_level_run_span() {
    let logs = CapturedLogs::default();
    let fixture =
        CompactionFixture::with_history_and_summary("span-probe-standalone")
            .await;
    fixture
        .kernel
        .compact_session(
            &fixture.session_id,
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("compact");

    let line = spans::line_with(
        &logs.content(),
        "scripted model streaming span-probe-standalone",
    )
    .expect("log line");
    assert_eq!(line.matches("run{").count(), 1, "{line}");
    let trace = spans::run_fields(&line)
        .0
        .expect("trace on the summary call");
    assert!(trace.starts_with("trace-"), "{line}");
}

/// `/compact` reuses its slash command's Run instead of opening another span.
#[tokio::test]
async fn manual_compact_command_opens_no_extra_span() {
    let logs = CapturedLogs::default();
    let fixture =
        CompactionFixture::with_history_and_summary("span-probe-manual").await;
    fixture
        .kernel
        .run_with_insertion(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/compact".into(),
            },
            Arc::new(RecordingSink::default()),
            None,
        )
        .await
        .expect("compact command");

    let line = spans::line_with(
        &logs.content(),
        "scripted model streaming span-probe-manual",
    )
    .expect("log line");
    assert_eq!(line.matches("run{").count(), 1, "{line}");
    assert!(spans::run_fields(&line).0.is_some(), "{line}");
}

/// A Tree navigation summary is a top-level Run with a trace of its own.
#[tokio::test]
async fn branch_summary_opens_a_top_level_run_span() {
    let logs = CapturedLogs::default();
    let fixture =
        CompactionFixture::with_history_and_summary("span-probe-branch").await;
    let target = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("tree before navigation")
        .entries
        .into_iter()
        .find(|entry| {
            entry.kind == "message"
                && entry.payload["content"]["type"] == "user"
        })
        .expect("first user entry")
        .entry_id;
    fixture
        .kernel
        .navigate_session_with_summary(
            &fixture.session_id,
            kernel::TreeNavigationRequest {
                target,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("navigate with summary");

    let line = spans::line_with(
        &logs.content(),
        "scripted model streaming span-probe-branch",
    )
    .expect("log line");
    assert_eq!(line.matches("run{").count(), 1, "{line}");
    let trace = spans::run_fields(&line)
        .0
        .expect("trace on the summary call");
    assert!(trace.starts_with("trace-"), "{line}");
    // The Run's start line ties its new trace to the Session.
    let start = spans::line_with(
        &logs.content(),
        &format!("started branch summary for session {}", fixture.session_id),
    )
    .expect("start line");
    assert_eq!(
        spans::run_fields(&start),
        spans::run_fields(&line),
        "{start}"
    );
}

/// Extension observations and decisions shared by compaction hook tests.
#[derive(Default)]
struct HookState {
    /// Cancels every compaction from `session_before_compact` when set.
    cancel_compaction: AtomicBool,
    /// Every `session_before_compact` event, in delivery order.
    before_compact: Mutex<Vec<protocol::SessionBeforeCompactEvent>>,
    /// Summary of an extension-built compaction that reuses the prepared tail.
    replacement_summary: Mutex<Option<String>>,
    /// Every `session_compact_failed` event, in delivery order.
    compact_failed: Mutex<Vec<protocol::SessionCompactFailedEvent>>,
    /// Branch summary an extension supplies on tree navigation, if any.
    tree_summary: Mutex<Option<String>>,
    /// Every `session_before_tree` event, in delivery order.
    before_tree: Mutex<Vec<protocol::SessionBeforeTreeEvent>>,
}

impl HookState {
    /// Wraps this state in a static extension factory with one hook module.
    fn extensions(self: &Arc<Self>) -> StaticExtensionFactory {
        let state = Arc::clone(self);
        StaticExtensionFactory::new(
            protocol::StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(HookModule(Arc::clone(&state)))
                    as Arc<dyn ExtensionModule>)
            })],
        )
    }
}

/// Extension module registering the compaction hooks under test.
struct HookModule(Arc<HookState>);

impl ExtensionModule for HookModule {
    /// Identifies the test hook owner.
    fn descriptor(&self) -> protocol::ExtensionDescriptor {
        protocol::ExtensionDescriptor {
            id: protocol::ExtensionId::try_from("compaction-hooks")
                .expect("extension id"),
            name: "Compaction hooks".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers every hook that shares this state.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<SessionBeforeCompactPoint, _>(HookHandler(
            Arc::clone(&self.0),
        ))?;
        registrar.on::<SessionCompactFailedPoint, _>(HookHandler(
            Arc::clone(&self.0),
        ))?;
        registrar
            .on::<SessionBeforeTreePoint, _>(HookHandler(Arc::clone(&self.0)))
    }
}

/// Handler that records hook events and applies the configured decisions.
struct HookHandler(Arc<HookState>);

#[async_trait]
impl ExtensionHandler<SessionBeforeCompactPoint> for HookHandler {
    /// Records the event and cancels when the test asks for it.
    async fn handle(
        &self,
        event: &protocol::SessionBeforeCompactEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::SessionBeforeCompactResult, ExtensionError> {
        self.0
            .before_compact
            .lock()
            .expect("hook lock")
            .push(event.clone());
        // An extension can build a complete compaction from the prepared
        // partition, echoing the retained tail back.
        let compaction = self
            .0
            .replacement_summary
            .lock()
            .expect("hook lock")
            .clone()
            .map(|summary| {
                protocol::CompactionData::builder()
                    .summary(summary)
                    .retained_tail(event.preparation.retained_tail.clone())
                    .tokens_before(event.preparation.tokens_before)
                    .build()
            });
        Ok(protocol::SessionBeforeCompactResult {
            cancel: self.0.cancel_compaction.load(Ordering::Relaxed),
            compaction,
        })
    }
}

/// One Tool Turn reading `input.txt` whose reported usage is `total_tokens`.
fn tool_turn(call_id: &str, total_tokens: u64) -> Vec<ModelStreamEvent> {
    vec![
        ModelStreamEvent::ToolCall(
            ToolCall::builder()
                .tool_call_id(ToolCallId::try_from(call_id).expect("call id"))
                .name("read".to_string())
                .arguments(serde_json::json!({ "path": "input.txt" }))
                .build(),
        ),
        ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(StopReason::ToolUse)
                .raw_stop_reason(None)
                .usage(protocol::ModelUsage::from(
                    protocol::Usage::new()
                        .input_tokens(total_tokens.saturating_sub(1_000))
                        .output_tokens(1_000.min(total_tokens))
                        .cached_input_tokens(0)
                        .cache_creation_input_tokens(0)
                        .total_tokens(total_tokens),
                ))
                .build(),
        ),
    ]
}

/// One complete text answer with the default small usage.
fn text_answer(text: &str) -> Result<Vec<ModelStreamEvent>, ModelError> {
    Ok(vec![
        ModelStreamEvent::TextDelta(text.to_string()),
        ScriptedModel::finished(StopReason::EndTurn),
    ])
}

/// Runs one prompt on a threshold kernel and returns the Run and its events.
async fn run_prompt(
    kernel: &Kernel,
    session_id: &SessionId,
    input: &str,
) -> (Result<RunResult, kernel::KernelError>, Vec<AgentEvent>) {
    let sink = Arc::new(RecordingSink::default());
    let result = kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: input.into(),
            },
            sink.clone(),
        )
        .await;
    (result, sink.events())
}

/// Returns the settled outcome reported by a Run's events.
fn settled_outcome(events: &[AgentEvent]) -> Option<protocol::AgentOutcome> {
    events.iter().find_map(|event| match &event.payload {
        AgentEventPayload::AgentSettled { outcome, .. } => {
            Some(outcome.clone())
        }
        _ => None,
    })
}

/// Counts `CompactionStart` events in a Run.
fn compaction_starts(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event.payload, AgentEventPayload::CompactionStart { .. })
        })
        .count()
}

/// A Tool Turn over the threshold compacts before the next model request of the same Run.
#[tokio::test]
async fn tool_turn_over_threshold_compacts_before_next_turn() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(tool_turn("call-1", 84_000)),
            text_answer("mid-run summary"),
            text_answer("final answer"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-mid-run").expect("session id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;

    let (result, events) = run_prompt(&kernel, &session_id, "work").await;

    assert_eq!(result.expect("run").turns.len(), 2);
    let position = |matches: &dyn Fn(&AgentEventPayload) -> bool| {
        events
            .iter()
            .position(|event| matches(&event.payload))
            .expect("event")
    };
    let first_turn_end = position(&|payload| {
        matches!(payload, AgentEventPayload::TurnEnd { .. })
    });
    let compaction = position(&|payload| {
        matches!(
            payload,
            AgentEventPayload::CompactionStart {
                reason: protocol::CompactionReason::Threshold,
                ..
            }
        )
    });
    let second_turn_start = events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(event.payload, AgentEventPayload::TurnStart { .. })
        })
        .nth(1)
        .map(|(index, _)| index)
        .expect("second TurnStart");
    assert!(first_turn_end < compaction && compaction < second_turn_start);
    let requests = model.requests.lock().expect("request lock");
    assert_eq!(requests.len(), 3);
    assert!(matches!(
        requests[2].messages.get(1).map(|message| &message.content),
        Some(MessageContent::CompactionSummary { .. })
    ));
}

/// A Tool Turn below the threshold continues without compaction.
#[tokio::test]
async fn tool_turn_below_threshold_does_not_compact() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(tool_turn("call-1", 1_000)),
            text_answer("final answer"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-mid-run-small").expect("session id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;

    let (result, events) = run_prompt(&kernel, &session_id, "work").await;

    assert_eq!(result.expect("run").turns.len(), 2);
    assert_eq!(model.requests.lock().expect("request lock").len(), 2);
    assert_eq!(compaction_starts(&events), 0);
}

/// A failed Run-internal summary is reported and the Run keeps going.
#[tokio::test]
async fn run_internal_summary_failure_keeps_the_run_going() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(tool_turn("call-1", 84_000)),
            Err(ModelError::Request(
                ModelFailure::builder()
                    .summary("summary failed".to_string())
                    .status(None)
                    .retry_disposition(ModelRetryDisposition::NonRetryable)
                    .build(),
            )),
            text_answer("final answer"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-mid-run-failure").expect("session id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;

    let (result, events) = run_prompt(&kernel, &session_id, "work").await;

    assert_eq!(result.expect("run").turns.len(), 2);
    assert_eq!(
        settled_outcome(&events),
        Some(protocol::AgentOutcome::Succeeded)
    );
    assert!(events.iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionEnd {
            outcome: protocol::CompactionOutcome::Failed { .. },
            ..
        }
    )));
}

/// An extension that cancels a Run-internal compaction does not stop the Run
/// or produce a second warning.
#[tokio::test]
async fn run_internal_extension_cancel_keeps_the_run_going_without_warn() {
    let logs = CapturedLogs::default();
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(tool_turn("call-1", 84_000)),
            text_answer("final answer"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-mid-run-cancel-hook").expect("session id");
    let hooks = Arc::new(HookState::default());
    hooks.cancel_compaction.store(true, Ordering::Relaxed);
    let kernel = threshold_kernel_with(
        root.path(),
        &model,
        0,
        &session_id,
        hooks.extensions(),
    )
    .await;

    let (result, events) = run_prompt(&kernel, &session_id, "work").await;

    assert_eq!(result.expect("run").turns.len(), 2);
    assert_eq!(
        settled_outcome(&events),
        Some(protocol::AgentOutcome::Succeeded)
    );
    assert!(!hooks.before_compact.lock().expect("hook lock").is_empty());
    assert!(!logs.content().lines().any(|line| {
        line.contains("continuing Run without")
            && line.contains("session compaction cancelled")
    }));
}

/// Cancelling a Run-internal compaction ends the Run as cancelled.
#[tokio::test]
async fn run_internal_cancellation_ends_the_run_cancelled() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(tool_turn("call-1", 84_000)),
            Err(ModelError::Cancelled),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-mid-run-cancelled").expect("session id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;

    let (_result, events) = run_prompt(&kernel, &session_id, "work").await;

    assert_eq!(
        settled_outcome(&events),
        Some(protocol::AgentOutcome::Cancelled)
    );
    // The cancellation came from the Run-internal compaction, not a Turn.
    assert!(events.iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionEnd {
            reason: protocol::CompactionReason::Threshold,
            outcome: protocol::CompactionOutcome::Cancelled,
            ..
        }
    )));
    assert!(
        kernel
            .session_tree(&session_id)
            .expect("tree")
            .entries
            .iter()
            .all(|entry| entry.kind != "compaction")
    );
}

/// A Run-internal compaction is not repeated by the post-Run check.
#[tokio::test]
async fn run_internal_compaction_is_not_repeated_after_the_run() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(tool_turn("call-1", 84_000)),
            text_answer("mid-run summary"),
            text_answer("final answer"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-mid-run-once").expect("session id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;

    let (result, events) = run_prompt(&kernel, &session_id, "work").await;

    result.expect("run");
    assert_eq!(compaction_starts(&events), 1);
}

/// A Tool loop with nothing compactable never loops or warns.
#[tokio::test]
async fn tool_loop_without_compactable_context_never_loops() {
    let logs = CapturedLogs::default();
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(tool_turn("call-1", 84_000)),
            Ok(tool_turn("call-2", 84_000)),
            Ok(tool_turn("call-3", 84_000)),
            // The last answer also crosses the threshold, so the post-Run
            // check meets the same empty compaction.
            Ok(large_usage_answer()),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-tool-loop").expect("session id");
    // A recent window larger than the whole history leaves nothing to summarize.
    let kernel =
        threshold_kernel(root.path(), &model, 1_000_000, &session_id).await;

    let (result, events) = run_prompt(&kernel, &session_id, "work").await;

    assert_eq!(result.expect("run").turns.len(), 4);
    assert_eq!(model.requests.lock().expect("request lock").len(), 4);
    assert_eq!(compaction_starts(&events), 0);
    assert!(!logs.content().lines().any(|line| {
        line.contains("WARN")
            && line.contains("session-tool-loop")
            && line.contains("no model context to compact")
    }));
}

/// Builds a threshold-style Kernel whose compaction config carries `overrides`
/// and creates `session-override` when the build succeeds.
async fn override_kernel(
    root: &Path,
    model: &Arc<ScriptedModel>,
    overrides: &[(&str, u64)],
) -> Result<Kernel, kernel::KernelError> {
    let cwd = root.join("workspace/override");
    std::fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(80_000)));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(
            Arc::clone(model) as Arc<dyn Model>
        )))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root,
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
        .compaction_policy(config::CompactionConfig {
            policy: CompactionPolicy::builder()
                .enabled(true)
                .reserve_tokens(16_384)
                .keep_recent_tokens(0)
                .build(),
            model_overrides: overrides
                .iter()
                .map(|(key, reserve_tokens)| {
                    (
                        (*key).to_string(),
                        config::CompactionModelOverride {
                            reserve_tokens: Some(*reserve_tokens),
                            keep_recent_tokens: None,
                        },
                    )
                })
                .collect(),
        })
        .build()
        .build()?;
    kernel
        .create_session(SessionCreateOptions {
            session_id: SessionId::try_from("session-override")
                .expect("session id"),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    Ok(kernel)
}

/// An override naming no catalog model fails the Kernel build.
#[tokio::test]
async fn unknown_compaction_override_fails_kernel_build() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::new()),
        requests: Mutex::new(Vec::new()),
    });

    let error =
        override_kernel(root.path(), &model, &[("fixture/missing", 8_000)])
            .await
            .err()
            .expect("unknown override");

    assert!(error.to_string().contains("fixture/missing"), "{error}");
}

/// An overridden reserve that fills the window fails the Kernel build.
#[tokio::test]
async fn override_reserve_at_or_above_window_fails_kernel_build() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::new()),
        requests: Mutex::new(Vec::new()),
    });

    let error = override_kernel(
        root.path(),
        &model,
        &[("fixture/compaction", 100_000)],
    )
    .await
    .err()
    .expect("reserve fills the window");

    assert!(
        error.to_string().contains("reserve_tokens=100000"),
        "{error}"
    );
}

/// An overridden reserve lowers the threshold for that model only.
#[tokio::test]
async fn override_reserve_changes_the_threshold() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Ok(vec![
                ModelStreamEvent::TextDelta("medium answer".to_string()),
                ModelStreamEvent::Finished(
                    ModelFinal::builder()
                        .stop_reason(StopReason::EndTurn)
                        .raw_stop_reason(None)
                        .usage(protocol::ModelUsage::from(
                            protocol::Usage::new()
                                .input_tokens(14_000)
                                .output_tokens(1_000)
                                .cached_input_tokens(0)
                                .cache_creation_input_tokens(0)
                                .total_tokens(15_000),
                        ))
                        .build(),
                ),
            ]),
            text_answer("override summary"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    // The 100k window minus a 90k reserve compacts above 10k tokens.
    let kernel =
        override_kernel(root.path(), &model, &[("fixture/compaction", 90_000)])
            .await
            .expect("build kernel");
    let session_id = SessionId::try_from("session-override").expect("id");

    let (result, events) = run_prompt(&kernel, &session_id, "context").await;

    result.expect("run");
    assert!(events.iter().any(|event| matches!(
        event.payload,
        AgentEventPayload::CompactionEnd {
            reason: protocol::CompactionReason::Threshold,
            ..
        }
    )));
    assert_eq!(model.requests.lock().expect("request lock").len(), 2);
}

/// The before-compact hook sees the prepared partition and the caller's instructions.
#[tokio::test]
async fn before_compact_event_carries_preparation_and_instructions() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            text_answer("first answer"),
            text_answer("second answer"),
            text_answer("history summary"),
            text_answer("prefix summary"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-hook-preparation").expect("session id");
    let hooks = Arc::new(HookState::default());
    let kernel = threshold_kernel_with(
        root.path(),
        &model,
        0,
        &session_id,
        hooks.extensions(),
    )
    .await;
    for input in ["first question", "second question", "/compact keep names"] {
        run_prompt(&kernel, &session_id, input)
            .await
            .0
            .expect(input);
    }

    let events = hooks.before_compact.lock().expect("hook lock");
    let event = events.first().expect("before-compact event");
    assert_eq!(event.custom_instructions.as_deref(), Some("keep names"));
    let preparation = &event.preparation;
    assert!(preparation.is_split_turn);
    assert_eq!(preparation.turn_prefix_messages.len(), 1);
    assert_eq!(preparation.retained_tail.len(), 1);
    assert!(preparation.messages_to_summarize.iter().any(|message| {
        message.content.text_content().as_deref() == Some("first question")
    }));
    assert_eq!(
        preparation.settings,
        CompactionPolicy::builder()
            .enabled(true)
            .reserve_tokens(16_384)
            .keep_recent_tokens(0)
            .build()
    );
}

/// An extension compaction built from the prepared retained tail is persisted.
#[tokio::test]
async fn extension_compaction_built_from_preparation_is_persisted() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            text_answer("first answer"),
            text_answer("second answer"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-hook-replacement").expect("session id");
    let hooks = Arc::new(HookState::default());
    *hooks.replacement_summary.lock().expect("hook lock") =
        Some("ext".to_string());
    let kernel = threshold_kernel_with(
        root.path(),
        &model,
        0,
        &session_id,
        hooks.extensions(),
    )
    .await;
    for input in ["first question", "second question"] {
        run_prompt(&kernel, &session_id, input)
            .await
            .0
            .expect(input);
    }

    let result = kernel
        .compact_session(&session_id, Arc::new(RecordingSink::default()))
        .await
        .expect("extension compaction");

    assert_eq!(result.summary, "ext");
    let retained = hooks.before_compact.lock().expect("hook lock")[0]
        .preparation
        .retained_tail
        .clone();
    let active = kernel.session_messages(&session_id).expect("history");
    assert!(matches!(
        active.first().map(|message| &message.content),
        Some(MessageContent::CompactionSummary { compaction })
            if compaction.summary == "ext"
    ));
    assert_eq!(&active[1..], retained.as_slice());
    assert_eq!(model.requests.lock().expect("request lock").len(), 2);
}

#[async_trait]
impl ExtensionHandler<SessionCompactFailedPoint> for HookHandler {
    /// Records each failed or aborted compaction.
    async fn handle(
        &self,
        event: &protocol::SessionCompactFailedEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        self.0
            .compact_failed
            .lock()
            .expect("hook lock")
            .push(event.clone());
        Ok(())
    }
}

/// Runs two Turns on a hooked threshold kernel, then compacts manually.
async fn manual_compaction_with_hooks(
    session: &str,
    summary_response: Result<Vec<ModelStreamEvent>, ModelError>,
    hooks: &Arc<HookState>,
) -> Result<protocol::CompactionResult, kernel::KernelError> {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            text_answer("first answer"),
            text_answer("second answer"),
            summary_response,
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id = SessionId::try_from(session).expect("session id");
    let kernel = threshold_kernel_with(
        root.path(),
        &model,
        0,
        &session_id,
        hooks.extensions(),
    )
    .await;
    for input in ["first question", "second question"] {
        run_prompt(&kernel, &session_id, input)
            .await
            .0
            .expect(input);
    }
    kernel
        .compact_session(&session_id, Arc::new(RecordingSink::default()))
        .await
}

/// Returns the single recorded `session_compact_failed` event.
fn only_compact_failed(
    hooks: &HookState,
) -> protocol::SessionCompactFailedEvent {
    let events = hooks.compact_failed.lock().expect("hook lock");
    assert_eq!(events.len(), 1, "{events:?}");
    events.first().cloned().expect("one compact-failed event")
}

/// A failed summary reports a non-aborted compaction failure.
#[tokio::test]
async fn failed_summary_reports_compact_failed() {
    let hooks = Arc::new(HookState::default());
    let result = manual_compaction_with_hooks(
        "session-failed-hook",
        Err(ModelError::Request(
            ModelFailure::builder()
                .summary("summary failed".to_string())
                .status(None)
                .retry_disposition(ModelRetryDisposition::NonRetryable)
                .build(),
        )),
        &hooks,
    )
    .await;

    result.expect_err("compaction fails");
    let event = only_compact_failed(&hooks);
    assert_eq!(event.reason, protocol::CompactionReason::Manual);
    assert!(!event.aborted);
    assert!(!event.will_retry);
    assert!(!event.from_extension);
    assert!(
        event
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("summary failed")),
        "{event:?}"
    );
}

/// A cancelled summary reports an aborted compaction.
#[tokio::test]
async fn cancelled_summary_reports_aborted_compact_failed() {
    let hooks = Arc::new(HookState::default());
    let result = manual_compaction_with_hooks(
        "session-cancelled-hook",
        Err(ModelError::Cancelled),
        &hooks,
    )
    .await;

    result.expect_err("compaction fails");
    let event = only_compact_failed(&hooks);
    assert!(event.aborted);
    assert_eq!(event.error_message, None);
}

/// An extension cancelling the compaction reports an aborted compaction.
#[tokio::test]
async fn extension_cancel_reports_aborted_compact_failed() {
    let hooks = Arc::new(HookState::default());
    hooks.cancel_compaction.store(true, Ordering::Relaxed);
    let result = manual_compaction_with_hooks(
        "session-hook-cancel-report",
        text_answer("unused summary"),
        &hooks,
    )
    .await;

    result.expect_err("compaction fails");
    let event = only_compact_failed(&hooks);
    assert!(event.aborted);
    assert_eq!(event.reason, protocol::CompactionReason::Manual);
}

/// A manual compaction with nothing to compact reports a failure.
#[tokio::test]
async fn manual_empty_compaction_reports_compact_failed() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::new()),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-empty-hook").expect("session id");
    let hooks = Arc::new(HookState::default());
    let kernel = threshold_kernel_with(
        root.path(),
        &model,
        0,
        &session_id,
        hooks.extensions(),
    )
    .await;

    let result = kernel
        .compact_session(&session_id, Arc::new(RecordingSink::default()))
        .await;

    result.expect_err("compaction fails");
    let event = only_compact_failed(&hooks);
    assert_eq!(event.reason, protocol::CompactionReason::Manual);
    assert!(!event.aborted);
    assert!(event.error_message.is_some());
}

/// An automatic compaction with nothing to compact stays silent, as in pi.
#[tokio::test]
async fn automatic_empty_compaction_reports_nothing() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([Ok(large_usage_answer())])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-auto-empty-hook").expect("session id");
    let hooks = Arc::new(HookState::default());
    let kernel = threshold_kernel_with(
        root.path(),
        &model,
        1_000_000,
        &session_id,
        hooks.extensions(),
    )
    .await;

    run_prompt(&kernel, &session_id, "large context")
        .await
        .0
        .expect("run");

    assert!(hooks.compact_failed.lock().expect("hook lock").is_empty());
}

/// A second provider overflow after one recovery reports pi's recovery failure.
#[tokio::test]
async fn second_overflow_reports_recovery_failure_text() {
    let hooks = Arc::new(HookState::default());
    let fixture = OverflowFixture::with_extensions(
        [
            Err(ScriptedModel::context_overflow()),
            text_answer("compact summary"),
            Err(ScriptedModel::context_overflow()),
        ],
        0,
        hooks.extensions(),
    )
    .await;

    fixture.run().await;

    let event = only_compact_failed(&hooks);
    assert_eq!(event.reason, protocol::CompactionReason::Overflow);
    assert!(!event.will_retry);
    assert!(!event.aborted);
    assert_eq!(
        event.error_message.as_deref(),
        Some(
            "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model."
        )
    );
}

/// A second recoverable truncation after one recovery reports pi's truncation text.
#[tokio::test]
async fn second_truncation_reports_truncated_recovery_text() {
    let hooks = Arc::new(HookState::default());
    // Output below the request budget means the window, not the budget, ran out.
    let truncated = || {
        Ok(vec![
            ModelStreamEvent::TextDelta("partial".to_string()),
            ScriptedModel::finished(StopReason::MaxTokens),
        ])
    };
    let fixture = OverflowFixture::with_extensions(
        [truncated(), text_answer("compact summary"), truncated()],
        0,
        hooks.extensions(),
    )
    .await;

    fixture.run().await;

    let event = only_compact_failed(&hooks);
    assert_eq!(event.reason, protocol::CompactionReason::Overflow);
    assert_eq!(
        event.error_message.as_deref(),
        Some(
            "Truncated response recovery failed after one compact-and-retry attempt."
        )
    );
}

/// An overflow with nothing to compact ends without a recovery-failure report.
#[tokio::test]
async fn overflow_without_compactable_context_reports_nothing() {
    let hooks = Arc::new(HookState::default());
    let fixture = OverflowFixture::with_extensions(
        [Err(ScriptedModel::context_overflow())],
        1_000_000,
        hooks.extensions(),
    )
    .await;

    fixture.run().await;

    assert!(hooks.compact_failed.lock().expect("hook lock").is_empty());
}

#[async_trait]
impl ExtensionHandler<SessionBeforeTreePoint> for HookHandler {
    /// Records the event and supplies the configured branch summary.
    async fn handle(
        &self,
        event: &protocol::SessionBeforeTreeEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::SessionBeforeTreeResult, ExtensionError> {
        self.0
            .before_tree
            .lock()
            .expect("hook lock")
            .push(event.clone());
        let summary = self.0.tree_summary.lock().expect("hook lock").clone();
        Ok(protocol::SessionBeforeTreeResult::builder()
            .summary(summary.map(|summary| protocol::TreeSummary {
                summary,
                // Extension details have an arbitrary shape.
                details: Some(serde_json::json!({ "source": "x" })),
                usage: None,
            }))
            .build())
    }
}

/// Returns the id of the first persisted entry whose message content has `kind`.
fn first_message_entry(
    kernel: &Kernel,
    session_id: &SessionId,
    kind: &str,
) -> protocol::EntryId {
    kernel
        .session_tree(session_id)
        .expect("tree")
        .entries
        .into_iter()
        .find(|entry| {
            entry.kind == "message"
                && entry
                    .payload
                    .get("content")
                    .and_then(|content| content.get("type"))
                    .and_then(serde_json::Value::as_str)
                    == Some(kind)
        })
        .expect("message entry")
        .entry_id
}

/// Returns the branch summaries in a Session's active model context.
fn branch_summaries(messages: &[AgentMessage]) -> Vec<&AgentMessage> {
    messages
        .iter()
        .filter(|message| {
            matches!(message.content, MessageContent::BranchSummary { .. })
        })
        .collect()
}

/// Builds a hooked kernel with two Turns whose extension supplies "ext branch summary".
async fn branch_summary_kernel(
    root: &Path,
    session: &str,
    keep_recent_tokens: u64,
    later_responses: impl IntoIterator<
        Item = Result<Vec<ModelStreamEvent>, ModelError>,
    >,
) -> (Kernel, Arc<ScriptedModel>, SessionId, Arc<HookState>) {
    let mut responses = VecDeque::from([
        text_answer("first answer"),
        text_answer("abandoned answer"),
    ]);
    responses.extend(later_responses);
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(responses),
        requests: Mutex::new(Vec::new()),
    });
    let session_id = SessionId::try_from(session).expect("session id");
    let hooks = Arc::new(HookState::default());
    *hooks.tree_summary.lock().expect("hook lock") =
        Some("ext branch summary".to_string());
    let kernel = threshold_kernel_with(
        root,
        &model,
        keep_recent_tokens,
        &session_id,
        hooks.extensions(),
    )
    .await;
    for input in ["first question", "abandoned question"] {
        run_prompt(&kernel, &session_id, input)
            .await
            .0
            .expect(input);
    }
    (kernel, model, session_id, hooks)
}

/// A navigation branch summary enters the next model request and the active context.
#[tokio::test]
async fn branch_summary_enters_the_next_model_request() {
    let root = tempfile::tempdir().expect("store root");
    let (kernel, model, session_id, _hooks) = branch_summary_kernel(
        root.path(),
        "session-branch-context",
        0,
        [text_answer("next answer")],
    )
    .await;
    let target = first_message_entry(&kernel, &session_id, "assistant");
    kernel
        .navigate_session_with_summary(
            &session_id,
            kernel::TreeNavigationRequest {
                target,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("navigate with summary");

    run_prompt(&kernel, &session_id, "next question")
        .await
        .0
        .expect("next prompt");

    let requests = model.requests.lock().expect("request lock");
    let request = requests.last().expect("next request");
    assert!(request.messages.iter().any(|message| matches!(
        &message.content,
        MessageContent::BranchSummary { branch }
            if branch.summary == "ext branch summary"
    )));
    let active = kernel.session_messages(&session_id).expect("history");
    let summaries = branch_summaries(&active);
    assert_eq!(summaries.len(), 1);
    let summary = summaries.first().expect("branch summary");
    let MessageContent::BranchSummary { branch } = &summary.content else {
        unreachable!("filtered to branch summaries");
    };
    let expected = format!("branch-summary-{}", branch.entry_id);
    assert_eq!(summary.identity.message_id.as_str(), expected);
    assert_eq!(summary.identity.turn_id.as_str(), expected);
}

/// A reopened Session rebuilds the same branch summary context.
#[tokio::test]
async fn reopened_session_rebuilds_the_same_branch_summary_context() {
    let root = tempfile::tempdir().expect("store root");
    let (kernel, _model, session_id, _hooks) =
        branch_summary_kernel(root.path(), "session-branch-reopen", 0, [])
            .await;
    let target = first_message_entry(&kernel, &session_id, "assistant");
    kernel
        .navigate_session_with_summary(
            &session_id,
            kernel::TreeNavigationRequest {
                target,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("navigate with summary");
    let live = kernel.session_messages(&session_id).expect("live history");

    kernel.close_session(&session_id).await.expect("close");
    kernel
        .resume_session(
            session_id.clone(),
            root.path().join("workspace").join(session_id.as_str()),
        )
        .await
        .expect("resume");

    let rebuilt = kernel.session_messages(&session_id).expect("rebuilt");
    assert_eq!(branch_summaries(&live).len(), 1);
    assert_eq!(live, rebuilt);
}

/// A branch summary kept in a compaction's retained tail is not duplicated on replay.
#[tokio::test]
async fn branch_summary_in_retained_tail_is_not_duplicated_after_compaction() {
    let root = tempfile::tempdir().expect("store root");
    // A 10-token budget keeps the branch summary (5) and the next Turn (4 + 3)
    // while summarizing the first Turn.
    let (kernel, _model, session_id, _hooks) = branch_summary_kernel(
        root.path(),
        "session-branch-retained",
        10,
        [text_answer("next answer"), text_answer("checkpoint")],
    )
    .await;
    let target = first_message_entry(&kernel, &session_id, "assistant");
    kernel
        .navigate_session_with_summary(
            &session_id,
            kernel::TreeNavigationRequest {
                target,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("navigate with summary");
    run_prompt(&kernel, &session_id, "next question")
        .await
        .0
        .expect("next prompt");
    kernel
        .compact_session(&session_id, Arc::new(RecordingSink::default()))
        .await
        .expect("compact");
    kernel.close_session(&session_id).await.expect("close");
    kernel
        .resume_session(
            session_id.clone(),
            root.path().join("workspace").join(session_id.as_str()),
        )
        .await
        .expect("resume");

    let active = kernel.session_messages(&session_id).expect("history");
    assert!(matches!(
        active.first().map(|message| &message.content),
        Some(MessageContent::CompactionSummary { .. })
    ));
    assert_eq!(branch_summaries(&active).len(), 1);
}

/// An extension branch summary with arbitrary details loads with empty file lists.
#[tokio::test]
async fn legacy_branch_summary_entry_loads_with_empty_file_lists() {
    let root = tempfile::tempdir().expect("store root");
    let (kernel, _model, session_id, _hooks) =
        branch_summary_kernel(root.path(), "session-branch-legacy", 0, [])
            .await;
    let target = first_message_entry(&kernel, &session_id, "assistant");
    kernel
        .navigate_session_with_summary(
            &session_id,
            kernel::TreeNavigationRequest {
                target,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("navigate with summary");
    kernel.close_session(&session_id).await.expect("close");
    kernel
        .resume_session(
            session_id.clone(),
            root.path().join("workspace").join(session_id.as_str()),
        )
        .await
        .expect("resume");

    let active = kernel.session_messages(&session_id).expect("history");
    let summaries = branch_summaries(&active);
    let MessageContent::BranchSummary { branch } =
        &summaries.first().expect("branch summary").content
    else {
        unreachable!("filtered to branch summaries");
    };
    assert!(branch.read_files.is_empty());
    assert!(branch.modified_files.is_empty());
}

/// Returns the persisted summary text of the newest branch-summary entry.
fn last_branch_summary_payload(
    kernel: &Kernel,
    session_id: &SessionId,
) -> serde_json::Value {
    kernel
        .session_tree(session_id)
        .expect("tree")
        .entries
        .into_iter()
        .rev()
        .find(|entry| entry.kind == "branch_summary")
        .expect("branch summary entry")
        .payload
}

/// Returns the user text of the newest model request.
fn last_request_text(model: &ScriptedModel) -> String {
    let requests = model.requests.lock().expect("request lock");
    let request = requests.last().expect("request");
    request
        .messages
        .get(1)
        .and_then(|message| message.content.text_content())
        .expect("user prompt")
}

/// Generated branch summaries use pi's structured prompt and preamble.
#[tokio::test]
async fn branch_summary_prompt_uses_pi_structure() {
    let fixture =
        CompactionFixture::with_history_and_summary("branch body").await;
    let target =
        first_message_entry(&fixture.kernel, &fixture.session_id, "user");

    fixture
        .kernel
        .navigate_session_with_summary(
            &fixture.session_id,
            kernel::TreeNavigationRequest {
                target,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("navigate with summary");

    {
        let requests = fixture.model.requests.lock().expect("request lock");
        let request = requests.last().expect("summary request");
        assert!(
            request
                .messages
                .first()
                .and_then(|message| message.content.text_content())
                .is_some_and(|text| text
                    .starts_with("You are a context summarization assistant"))
        );
    }
    let prompt = last_request_text(&fixture.model);
    assert!(prompt.starts_with("<conversation>\n"), "{prompt}");
    assert!(
        prompt.contains(
            "Create a structured summary of this conversation branch"
        )
    );
    let payload =
        last_branch_summary_payload(&fixture.kernel, &fixture.session_id);
    assert_eq!(
        payload.get("summary").and_then(serde_json::Value::as_str),
        Some(
            "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\nbranch body"
        )
    );
}

/// The branch-summary reserve bounds which abandoned messages are summarized.
#[tokio::test]
async fn branch_summary_uses_configured_reserve() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            text_answer("historical answer"),
            text_answer("recent answer"),
            text_answer("branch body"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-branch-reserve").expect("session id");
    // The 100k window minus a 99,990-token reserve leaves a 10-token budget:
    // the second Turn (4 + 4) fits, the first Turn's answer does not.
    let kernel = threshold_kernel_configured(
        root.path(),
        &model,
        0,
        &session_id,
        KernelOptions {
            branch_summary: config::BranchSummaryConfig {
                reserve_tokens: 99_990,
            },
            ..KernelOptions::default()
        },
    )
    .await;
    for input in ["historical question", "recent question"] {
        run_prompt(&kernel, &session_id, input)
            .await
            .0
            .expect(input);
    }
    let target = first_message_entry(&kernel, &session_id, "user");

    kernel
        .navigate_session_with_summary(
            &session_id,
            kernel::TreeNavigationRequest {
                target,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("navigate with summary");

    let prompt = last_request_text(&model);
    assert!(prompt.contains("recent question"), "{prompt}");
    assert!(!prompt.contains("historical question"), "{prompt}");
}

/// Branch summaries record files from Tool calls and nested branch summaries.
#[tokio::test]
async fn branch_summary_records_cumulative_files() {
    let root = tempfile::tempdir().expect("store root");
    let call = |id: &str, name: &str, arguments: serde_json::Value| {
        ModelStreamEvent::ToolCall(
            ToolCall::builder()
                .tool_call_id(ToolCallId::try_from(id).expect("call id"))
                .name(name.to_string())
                .arguments(arguments)
                .build(),
        )
    };
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            text_answer("start answer"),
            Ok(vec![
                call("read-a", "read", serde_json::json!({ "path": "a.rs" })),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            text_answer("first branch answer"),
            text_answer("first summary"),
            Ok(vec![
                call(
                    "read-input",
                    "read",
                    serde_json::json!({ "path": "input.txt" }),
                ),
                call(
                    "write-out",
                    "write",
                    serde_json::json!({ "path": "out.txt", "content": "x" }),
                ),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            text_answer("second branch answer"),
            text_answer("second summary"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-branch-files").expect("session id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;
    std::fs::write(
        root.path()
            .join("workspace")
            .join(session_id.as_str())
            .join("a.rs"),
        "fn main() {}",
    )
    .expect("write a.rs");
    for input in ["start", "first branch"] {
        run_prompt(&kernel, &session_id, input)
            .await
            .0
            .expect(input);
    }
    let start_answer = first_message_entry(&kernel, &session_id, "assistant");
    kernel
        .navigate_session_with_summary(
            &session_id,
            kernel::TreeNavigationRequest {
                target: start_answer.clone(),
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("first navigation");
    run_prompt(&kernel, &session_id, "second branch")
        .await
        .0
        .expect("second branch");

    kernel
        .navigate_session_with_summary(
            &session_id,
            kernel::TreeNavigationRequest {
                target: start_answer,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("second navigation");

    let payload = last_branch_summary_payload(&kernel, &session_id);
    assert_eq!(
        payload.get("details"),
        Some(&serde_json::json!({
            "readFiles": ["a.rs", "input.txt"],
            "modifiedFiles": ["out.txt"],
        }))
    );
    assert!(
        payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|summary| summary.ends_with(
                "<read-files>\na.rs\ninput.txt\n</read-files>\n\n<modified-files>\nout.txt\n</modified-files>"
            )),
        "{payload}"
    );
}

/// A truncated branch summary fails navigation and leaves the leaf in place.
#[tokio::test]
async fn truncated_branch_summary_fails_navigation() {
    let fixture = CompactionFixture::new(
        [Ok(vec![
            ModelStreamEvent::TextDelta("partial".to_string()),
            ScriptedModel::finished(StopReason::MaxTokens),
        ])],
        true,
    )
    .await;
    let before = fixture
        .kernel
        .session_tree(&fixture.session_id)
        .expect("tree")
        .leaf_id;
    let target =
        first_message_entry(&fixture.kernel, &fixture.session_id, "user");

    let error = fixture
        .kernel
        .navigate_session_with_summary(
            &fixture.session_id,
            kernel::TreeNavigationRequest {
                target,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect_err("truncated branch summary");

    assert!(
        error.to_string().contains("Branch summarization failed:"),
        "{error}"
    );
    assert_eq!(
        fixture
            .kernel
            .session_tree(&fixture.session_id)
            .expect("tree")
            .leaf_id,
        before
    );
}

/// Caller instructions reach the tree hook and the generated summary prompt.
#[tokio::test]
async fn navigation_instructions_reach_hook_and_prompt() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            text_answer("first answer"),
            text_answer("second answer"),
            text_answer("branch body"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-branch-instructions").expect("id");
    let hooks = Arc::new(HookState::default());
    let kernel = threshold_kernel_with(
        root.path(),
        &model,
        0,
        &session_id,
        hooks.extensions(),
    )
    .await;
    for input in ["first question", "second question"] {
        run_prompt(&kernel, &session_id, input)
            .await
            .0
            .expect(input);
    }
    let target = first_message_entry(&kernel, &session_id, "user");

    kernel
        .navigate_session_with_summary(
            &session_id,
            kernel::TreeNavigationRequest {
                target,
                summarize: true,
                custom_instructions: Some("focus api".to_string()),
            },
        )
        .await
        .expect("navigate with summary");

    let events = hooks.before_tree.lock().expect("hook lock");
    assert_eq!(
        events
            .first()
            .and_then(|event| event.custom_instructions.as_deref()),
        Some("focus api")
    );
    assert!(
        last_request_text(&model).ends_with("\n\nAdditional focus: focus api")
    );
}

/// Navigating forward to a descendant abandons nothing, so no summary is written.
#[tokio::test]
async fn summarizing_navigation_to_a_descendant_writes_no_summary() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            text_answer("first answer"),
            text_answer("second answer"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id =
        SessionId::try_from("session-branch-descendant").expect("id");
    let kernel = threshold_kernel(root.path(), &model, 0, &session_id).await;
    for input in ["first question", "second question"] {
        run_prompt(&kernel, &session_id, input)
            .await
            .0
            .expect(input);
    }
    let leaf = kernel
        .session_tree(&session_id)
        .expect("tree")
        .leaf_id
        .expect("leaf");
    let first_answer = first_message_entry(&kernel, &session_id, "assistant");
    kernel
        .navigate_session(&session_id, Some(first_answer))
        .await
        .expect("navigate back without summary");

    kernel
        .navigate_session_with_summary(
            &session_id,
            kernel::TreeNavigationRequest {
                target: leaf,
                summarize: true,
                custom_instructions: None,
            },
        )
        .await
        .expect("navigate forward");

    assert!(
        kernel
            .session_tree(&session_id)
            .expect("tree")
            .entries
            .iter()
            .all(|entry| entry.kind != "branch_summary")
    );
    assert_eq!(model.requests.lock().expect("request lock").len(), 2);
}

/// Sequential ids whose Turn ids become invalid once `fail_turns` is set.
struct FailingTurnIds {
    inner: SequentialIds,
    fail_turns: Arc<AtomicBool>,
}

impl IdGenerator for FailingTurnIds {
    /// Returns an empty, invalid Turn id after the switch flips.
    fn next(&self, kind: IdKind) -> String {
        if kind == IdKind::Turn && self.fail_turns.load(Ordering::Relaxed) {
            return String::new();
        }
        self.inner.next(kind)
    }
}

/// A committed navigation still succeeds when its sink report cannot be built.
#[tokio::test]
async fn navigation_succeeds_when_branch_report_fails() {
    let root = tempfile::tempdir().expect("store root");
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            text_answer("first answer"),
            text_answer("second answer"),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let session_id = SessionId::try_from("session-report-failure").expect("id");
    let fail_turns = Arc::new(AtomicBool::new(false));
    let kernel = threshold_kernel_configured(
        root.path(),
        &model,
        0,
        &session_id,
        KernelOptions {
            id_generator: Arc::new(FailingTurnIds {
                inner: SequentialIds(AtomicU64::new(0)),
                fail_turns: Arc::clone(&fail_turns),
            }),
            ..KernelOptions::default()
        },
    )
    .await;
    for input in ["first question", "second question"] {
        run_prompt(&kernel, &session_id, input)
            .await
            .0
            .expect(input);
    }
    let target = first_message_entry(&kernel, &session_id, "assistant");
    fail_turns.store(true, Ordering::Relaxed);

    let tree = kernel
        .navigate_session(&session_id, Some(target.clone()))
        .await
        .expect("committed navigation succeeds");

    assert_eq!(tree.leaf_id, Some(target));
}
