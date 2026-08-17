use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

    /// Returns all delegated operation records.
    fn records(&self) -> Vec<SessionRecord> {
        self.inner.records()
    }

    /// Delegates session-name persistence.
    fn set_name(&mut self, name: Option<String>) -> Result<(), StoreError> {
        self.inner.set_name(name)
    }

    /// Returns the delegated session name.
    fn name(&self) -> Option<&str> {
        self.inner.name()
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
            [Err(ModelError::Request(ModelFailure {
                summary: "summary failed".to_string(),
                status: None,
                retry_disposition: ModelRetryDisposition::NonRetryable,
            }))],
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
        Self::new_with_keep_recent(compaction_responses, prepare_history, 0)
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
    let request = requests.get(1).expect("summary request");
    assert!(request.tools.is_empty());
    assert_eq!(request.options.max_tokens, Some(8_000));
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
    assert_eq!(entry.payload["usage"]["input_tokens"], "10");
    assert_eq!(entry.payload["usage"]["output_tokens"], "5");
    assert_eq!(entry.payload["details"]["turnId"], result.turn_id.as_str());
    assert_eq!(entry.payload["details"]["reason"], "manual");
    assert_eq!(entry.payload["details"]["readFiles"], serde_json::json!([]));
    assert_eq!(
        entry.payload["details"]["modifiedFiles"],
        serde_json::json!([])
    );
    assert_eq!(result.summary, "summary text");
    assert_eq!(
        result.usage.as_ref().expect("summary usage").total_tokens,
        15
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

/// Compaction records pi's read/write/edit file metadata and carries it across checkpoints.
#[tokio::test]
async fn compaction_preserves_cumulative_file_operations() {
    let fixture = CompactionFixture::new_with_scripted_history(
        [
            Ok(vec![
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: ToolCallId::try_from("read-call")
                        .expect("read call id"),
                    name: "read".to_string(),
                    arguments: serde_json::json!({ "path": "input.txt" }),
                }),
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: ToolCallId::try_from("write-call")
                        .expect("write call id"),
                    name: "write".to_string(),
                    arguments: serde_json::json!({
                        "path": "output.txt",
                        "content": "output"
                    }),
                }),
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: ToolCallId::try_from("edit-call")
                        .expect("edit call id"),
                    name: "edit".to_string(),
                    arguments: serde_json::json!({
                        "path": "input.txt",
                        "edits": [{
                            "oldText": "input",
                            "newText": "changed"
                        }]
                    }),
                }),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            Ok(vec![
                ModelStreamEvent::TextDelta("historical answer".to_string()),
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
        0,
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
    let follow_up = requests.get(3).expect("follow-up provider request");
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
            ModelStreamEvent::Finished(ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: None,
                usage: ModelUsage::builder()
                    .input_tokens(101_000)
                    .output_tokens(10)
                    .cache_read_tokens(0)
                    .cache_write_tokens(0)
                    .total_tokens(101_010)
                    .build(),
            }),
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

/// Replay preserves the durable message-compaction order and card identity.
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

    assert_eq!(replay.len(), fixture.messages_before.len() + 1);
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
            Err(ModelError::Stream(ModelFailure {
                summary: "connection terminated".to_string(),
                status: None,
                retry_disposition: ModelRetryDisposition::Retryable,
            })),
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
    let request = requests.get(3).expect("second summary request");
    let MessageContent::User { blocks } = &request.messages[1].content else {
        panic!("summary prompt must be a User message");
    };
    let prompt = blocks[0].text().expect("summary prompt text");
    assert!(
        prompt
            .contains("<previous-summary>\nfirst summary\n</previous-summary>")
    );
    assert!(prompt.contains("follow-up question"));
    assert!(prompt.contains("follow-up answer"));
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
    assert!(prompt.contains("This is the PREFIX of a turn"));
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
