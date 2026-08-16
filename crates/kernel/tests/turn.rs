use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use extension::{
    BeforeAgentStartPoint, ExtensionContext, ExtensionError, ExtensionHandler,
    ExtensionModule, ExtensionRegistrar, StaticExtensionFactory, ToolCallPoint,
    ToolExecutionUpdatePoint,
};
use futures::{Stream, stream};
use kernel::{EventSink, KernelFactory, Model, ModelError, ModelFactory};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, AgentOutcome, ContentBlock, EntryId,
    ExtensionDescriptor, ExtensionId, IdGenerator, IdKind, LaneId,
    MessageContent, ModelFailure, ModelFinal, ModelProfile, ModelRequest,
    ModelRetryDisposition, ModelStreamEvent, ModelUsage, RunRequest, SessionId,
    StopReason, TimestampMs, ToolBlock, ToolCall, ToolCallId, ToolCallResult,
    ToolDefinition, ToolResult,
};
use store::{
    Clock, EntryKind, JsonlStoreFactory, NewEntry, SessionCreateOptions,
    StoreFactory as SessionStoreFactory,
};
use tokio_util::sync::CancellationToken;
use tools::{
    AgentTool, BuiltinToolFactory, ToolError, ToolExecutionContext,
    ToolFactory, ToolRegistry,
};

type TestModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// Shared deterministic capabilities used by all local kernel test models.
static TEST_MODEL_PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("scripted".to_string())
        .display_name("Scripted test model".to_string())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .build()
});

struct StepClock(Mutex<u64>);

impl Clock for StepClock {
    /// Advances deterministic wall time for each kernel observation.
    fn now(&self) -> TimestampMs {
        let mut value = self.0.lock().expect("clock lock");
        *value += 1;
        TimestampMs::from(*value)
    }
}

struct SequentialIds(Mutex<u64>);

impl IdGenerator for SequentialIds {
    /// Produces readable, stable ids for event and persistence assertions.
    fn next(&self, kind: IdKind) -> String {
        let mut value = self.0.lock().expect("id lock");
        *value += 1;
        format!("{}-{}", kind.prefix(), *value)
    }
}

struct ScriptedModel {
    scripts: Mutex<VecDeque<ScriptedResponse>>,
}

/// One deterministic provider boundary result consumed by `ScriptedModel`.
enum ScriptedResponse {
    /// A response stream may interleave normal deltas and structured failures.
    Stream(Vec<Result<ModelStreamEvent, ModelError>>),
}

impl ScriptedResponse {
    /// Converts successful model events into a fallible scripted stream.
    fn events(events: Vec<ModelStreamEvent>) -> Self {
        Self::Stream(events.into_iter().map(Ok).collect())
    }
}

impl ScriptedModel {
    /// Builds a successful terminal event with explicit deterministic usage.
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
}

#[async_trait]
impl Model for ScriptedModel {
    /// Returns the shared capabilities for deterministic scripted requests.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that scripted test models require no external readiness work.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Streams the next scripted response without consulting a real provider.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        let response = self
            .scripts
            .lock()
            .expect("script lock")
            .pop_front()
            .ok_or_else(|| {
                ModelError::Request(ModelFailure {
                    summary: "missing script".to_string(),
                    status: None,
                    retry_disposition: ModelRetryDisposition::NonRetryable,
                })
            })?;
        match response {
            ScriptedResponse::Stream(events) => {
                Ok(Box::pin(stream::iter(events)))
            }
        }
    }
}

struct StaticModelFactory(Arc<dyn Model>);

impl ModelFactory for StaticModelFactory {
    /// Returns the shared deterministic model used by this kernel test.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0))
    }
}

/// Captures provider requests while consuming deterministic multi-Turn scripts.
struct SystemPromptCaptureModel {
    scripts: Mutex<VecDeque<ScriptedResponse>>,
    requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl Model for SystemPromptCaptureModel {
    /// Returns the shared profile used by System Prompt lifecycle assertions.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that the capture model has no external provider dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Captures one complete request and streams the next deterministic response.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        self.requests.lock().expect("request lock").push(request);
        let response = self
            .scripts
            .lock()
            .expect("script lock")
            .pop_front()
            .expect("System Prompt script");
        match response {
            ScriptedResponse::Stream(events) => {
                Ok(Box::pin(stream::iter(events)))
            }
        }
    }
}

/// Replaces the System Prompt for only the first Agent Run in one Session.
#[derive(Clone)]
struct RunScopedPromptOverride(Arc<AtomicUsize>);

#[async_trait]
impl ExtensionHandler<BeforeAgentStartPoint> for RunScopedPromptOverride {
    /// Returns one replacement on the first Run and no replacement thereafter.
    async fn handle(
        &self,
        _event: &protocol::BeforeAgentStartEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::BeforeAgentStartResult, ExtensionError> {
        let first_run = self.0.fetch_add(1, Ordering::SeqCst) == 0;
        Ok(protocol::BeforeAgentStartResult {
            messages: Vec::new(),
            system_prompt: first_run.then(|| "replacement-system".to_string()),
        })
    }
}

impl ExtensionModule for RunScopedPromptOverride {
    /// Declares the Run-scoped System Prompt fixture identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("run-prompt-override")
                .expect("extension id"),
            name: "Run Prompt override".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the pre-Agent replacement handler.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<BeforeAgentStartPoint, _>(self.clone())
    }
}

struct DelayedTextTool;

#[async_trait]
impl AgentTool for DelayedTextTool {
    /// Describes deterministic text and delay arguments used by ordering tests.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delayed_text".to_string(),
            description: "Return text after a requested delay.".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    /// Delays completion so sibling tool-event order differs from persistence order.
    async fn execute(
        &self,
        call: ToolCall,
        _context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let delay_ms = call
            .arguments
            .get("delay_ms")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: call.name.clone(),
                message: "delay_ms must be an unsigned integer".to_string(),
            })?;
        let text = call
            .arguments
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: call.name,
                message: "text must be a string".to_string(),
            })?
            .to_string();
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text { text }])
            .is_error(false)
            .build())
    }
}

struct DelayedToolFactory;

impl ToolFactory for DelayedToolFactory {
    /// Registers the single delayed tool used to observe concurrent completion.
    fn create(&self) -> Result<ToolRegistry, ToolError> {
        let mut registry = ToolRegistry::default();
        registry.register(Arc::new(DelayedTextTool))?;
        Ok(registry)
    }
}

struct StreamingTool;

#[async_trait]
impl AgentTool for StreamingTool {
    /// Describes the deterministic partial-output tool used by kernel event tests.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "streaming".to_string(),
            description: "Publish one partial result before completion."
                .to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    /// Publishes one replaceable snapshot before returning the final result.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.publish(
            ToolResult::builder()
                .tool_call_id(call.tool_call_id.clone())
                .blocks(vec![ContentBlock::Text {
                    text: "partial".to_string(),
                }])
                .is_error(false)
                .build(),
        );
        tokio::task::yield_now().await;
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: "final".to_string(),
            }])
            .is_error(false)
            .build())
    }
}

struct StreamingToolFactory;

impl ToolFactory for StreamingToolFactory {
    /// Registers the deterministic streaming tool for event ordering tests.
    fn create(&self) -> Result<ToolRegistry, ToolError> {
        let mut registry = ToolRegistry::default();
        registry.register(Arc::new(StreamingTool))?;
        Ok(registry)
    }
}

struct BurstUpdateTool;

#[async_trait]
impl AgentTool for BurstUpdateTool {
    /// Describes the high-frequency latest-value update fixture.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "burst_updates".to_string(),
            description: "Publish many replaceable snapshots synchronously."
                .to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    /// Publishes many snapshots without yielding so only the latest value matters.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        for sequence in 0..2_000 {
            context.publish(
                ToolResult::builder()
                    .tool_call_id(call.tool_call_id.clone())
                    .blocks(vec![ContentBlock::Text {
                        text: format!("snapshot-{sequence}"),
                    }])
                    .is_error(false)
                    .build(),
            );
        }
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: "final".to_string(),
            }])
            .is_error(false)
            .build())
    }
}

struct BurstUpdateToolFactory;

impl ToolFactory for BurstUpdateToolFactory {
    /// Registers the burst-update tool used to verify latest-value delivery.
    fn create(&self) -> Result<ToolRegistry, ToolError> {
        let mut registry = ToolRegistry::default();
        registry.register(Arc::new(BurstUpdateTool))?;
        Ok(registry)
    }
}

#[derive(Default)]
struct RecordingSink(Mutex<Vec<AgentEvent>>);

#[async_trait]
impl EventSink for RecordingSink {
    /// Records events in the exact order observed by a protocol adapter.
    async fn emit(&self, event: AgentEvent) -> Result<(), kernel::SinkError> {
        self.0.lock().expect("sink lock").push(event);
        Ok(())
    }
}

#[derive(Clone)]
struct RecordingExtension(Arc<Mutex<Vec<&'static str>>>);

/// Policy extension that blocks every tool before its implementation runs.
#[derive(Clone)]
struct BlockingToolExtension;

#[async_trait]
impl ExtensionHandler<ToolCallPoint> for BlockingToolExtension {
    /// Returns a typed policy block used to verify persisted tool outcomes.
    async fn handle(
        &self,
        _event: &protocol::ToolCallEvent,
        _context: &ExtensionContext,
    ) -> Result<ToolCallResult, ExtensionError> {
        Ok(ToolCallResult::Block(
            ToolBlock::builder()
                .reason("policy denied".to_string())
                .terminate(false)
                .build(),
        ))
    }
}

impl ExtensionModule for BlockingToolExtension {
    /// Declares the deterministic policy extension identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("tool-policy").expect("extension id"),
            name: "Tool policy".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the policy before tool validation and execution.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<ToolCallPoint, _>(self.clone())
    }
}

#[async_trait]
impl ExtensionHandler<ToolExecutionUpdatePoint> for RecordingExtension {
    /// Records each typed partial tool update while allowing execution to continue.
    async fn handle(
        &self,
        _event: &protocol::ToolExecutionUpdateEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        self.0
            .lock()
            .expect("extension lock")
            .push("tool_execution_update");
        Ok(())
    }
}

impl ExtensionModule for RecordingExtension {
    /// Declares the deterministic tool-observer extension identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("tool-recorder").expect("extension id"),
            name: "Tool recorder".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the partial tool-update observer used by this test.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<ToolExecutionUpdatePoint, _>(self.clone())
    }
}

struct PendingModel;

#[async_trait]
impl Model for PendingModel {
    /// Returns the shared capabilities for the pending cancellation model.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that the pending model has no external preflight dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Returns a stream that remains pending until kernel cancellation drops it.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        Ok(Box::pin(stream::pending()))
    }
}

struct FailingPreflightModel;

#[async_trait]
impl Model for FailingPreflightModel {
    /// Returns the shared capabilities for the preflight failure fixture.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Reproduces a provider readiness failure before the first model Turn.
    async fn preflight(&self) -> Result<(), ModelError> {
        Err(ModelError::Preflight(ModelFailure {
            summary: "provider unavailable".to_string(),
            status: None,
            retry_disposition: ModelRetryDisposition::NonRetryable,
        }))
    }

    /// Rejects streaming because preflight must stop the run first.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        Err(ModelError::Request(ModelFailure {
            summary: "stream must not be called".to_string(),
            status: None,
            retry_disposition: ModelRetryDisposition::NonRetryable,
        }))
    }
}

/// Preflight failure still closes the durable and streamed run lifecycle.
#[tokio::test]
async fn preflight_failure_emits_and_persists_terminal_run_state() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(900)));
    let store_factory =
        Arc::new(JsonlStoreFactory::new(temporary.path(), Arc::clone(&clock)));
    let model: Arc<dyn Model> = Arc::new(FailingPreflightModel);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(
            Arc::clone(&store_factory) as Arc<dyn SessionStoreFactory>
        )
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-preflight-failure").expect("session id");
    let path = kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let sink = Arc::new(RecordingSink::default());

    let error = kernel
        .run(
            RunRequest {
                session_id,
                input: "hello".to_string(),
            },
            sink.clone(),
        )
        .await
        .expect_err("preflight should fail");
    assert!(matches!(error, kernel::KernelError::Model(_)));

    let events = sink.0.lock().expect("sink lock");
    assert!(matches!(
        events.first().map(|event| &event.payload),
        Some(AgentEventPayload::RunStart { .. })
    ));
    assert!(matches!(
        events.iter().rev().nth(1).map(|event| &event.payload),
        Some(AgentEventPayload::RunEnd {
            outcome: AgentOutcome::Failed { .. },
            ..
        })
    ));
    assert!(matches!(
        events.last().map(|event| &event.payload),
        Some(AgentEventPayload::AgentSettled {
            outcome: AgentOutcome::Failed { .. },
            ..
        })
    ));
    drop(events);

    let persisted = store_factory.open(&path).expect("open session log");
    let records = persisted.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].kind, store::RecordKind::OperationStarted);
    assert_eq!(records[1].kind, store::RecordKind::OperationFinished);
    assert_eq!(records[1].payload["status"], "failed");
}

/// One streamed response is one Turn and preserves first/last output timing.
#[tokio::test]
async fn streamed_response_produces_one_timed_turn() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(1_000)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::from([ScriptedResponse::events(vec![
            ModelStreamEvent::TextDelta("hel".to_string()),
            ModelStreamEvent::TextDelta("lo".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ])])),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            temporary.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build kernel");
    let session_id = SessionId::try_from("session-1").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            // Project-context discovery requires a real working directory.
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let sink = Arc::new(RecordingSink::default());

    let result = kernel
        .run(
            RunRequest {
                session_id,
                input: "say hello".to_string(),
            },
            sink.clone(),
        )
        .await
        .expect("run kernel");

    assert_eq!(result.turns.len(), 1);
    assert_eq!(result.messages.len(), 2);
    let assistant = result.messages.last().expect("assistant message");
    assert_eq!(assistant.identity.turn_id, result.turns[0].identity.turn_id);
    assert!(assistant.timing.ended_at_ms > assistant.timing.started_at_ms);
    let MessageContent::Assistant { blocks, metadata } = &assistant.content
    else {
        panic!("assistant message expected");
    };
    assert_eq!(
        blocks,
        &vec![ContentBlock::Text {
            text: "hello".into()
        }]
    );
    assert_eq!(metadata.provider_id, "fixture");
    assert_eq!(metadata.model_id, "scripted");
    assert_eq!(metadata.usage.total_tokens, 15);

    let events = sink.0.lock().expect("sink lock");
    assert!(events.iter().all(|event| {
        event.metadata.turn_id == result.turns[0].identity.turn_id
            && event.metadata.timestamp_ms.get() > 1_000
    }));
    assert!(matches!(
        events.first().map(|event| &event.payload),
        Some(AgentEventPayload::RunStart { .. })
    ));
    // AgentSettled now follows the terminal RunEnd exactly once.
    assert!(matches!(
        events.iter().rev().nth(1).map(|event| &event.payload),
        Some(AgentEventPayload::RunEnd {
            outcome: AgentOutcome::Succeeded,
            ..
        })
    ));
    assert!(matches!(
        events.last().map(|event| &event.payload),
        Some(AgentEventPayload::AgentSettled {
            outcome: AgentOutcome::Succeeded,
            ..
        })
    ));
}

/// A pre-Agent replacement applies to every Turn of its Run and not later Runs.
#[tokio::test]
async fn system_prompt_override_is_scoped_to_the_complete_run() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(1_250)));
    let model = Arc::new(SystemPromptCaptureModel {
        scripts: Mutex::new(VecDeque::from([
            ScriptedResponse::events(vec![
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: ToolCallId::try_from("override-tool")
                        .expect("tool id"),
                    name: "delayed_text".to_string(),
                    arguments: serde_json::json!({
                        "delay_ms": 0,
                        "text": "tool result"
                    }),
                }),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            ScriptedResponse::events(vec![
                ModelStreamEvent::TextDelta("first run done".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
            ScriptedResponse::events(vec![
                ModelStreamEvent::TextDelta("second run done".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let extension_calls = Arc::new(AtomicUsize::new(0));
    let module_calls = Arc::clone(&extension_calls);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(
            Arc::clone(&model) as Arc<dyn Model>
        )))
        .tool_factory(Arc::new(DelayedToolFactory))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            temporary.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            protocol::StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(RunScopedPromptOverride(Arc::clone(&module_calls)))
                    as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-prompt-override").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create Session");

    kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "first run".to_string(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("first Run");
    kernel
        .run(
            RunRequest {
                session_id,
                input: "second run".to_string(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("second Run");

    let requests = model.requests.lock().expect("request lock");
    let system_prompts = requests
        .iter()
        .map(|request| {
            request
                .messages
                .iter()
                .find_map(|message| match &message.content {
                    MessageContent::System { blocks } => blocks[0].text(),
                    _ => None,
                })
                .expect("System Prompt")
        })
        .collect::<Vec<_>>();
    assert_eq!(system_prompts[0], "replacement-system");
    assert_eq!(system_prompts[1], "replacement-system");
    assert_ne!(system_prompts[2], "replacement-system");
    assert_eq!(extension_calls.load(Ordering::SeqCst), 2);
}

/// Stream failures persist complete Assistant messages with and without partial output.
#[tokio::test]
async fn stream_failures_persist_complete_error_assistants() {
    for partial in [Some("partial"), None] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(1_500)));
        let mut events = Vec::new();
        if let Some(text) = partial {
            events.push(Ok(ModelStreamEvent::TextDelta(text.to_string())));
        }
        events.push(Err(ModelError::Stream(ModelFailure {
            summary: "socket closed".to_string(),
            status: None,
            retry_disposition: ModelRetryDisposition::Retryable,
        })));
        let model: Arc<dyn Model> = Arc::new(ScriptedModel {
            scripts: Mutex::new(VecDeque::from([ScriptedResponse::Stream(
                events,
            )])),
        });
        let kernel = KernelFactory::builder()
            .model_factory(Arc::new(StaticModelFactory(model)))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                temporary.path(),
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                temporary.path().join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::default()))
            .clock(clock)
            .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
            .build()
            .build()
            .expect("build kernel");
        let session_id = SessionId::try_from(if partial.is_some() {
            "session-partial-failure"
        } else {
            "session-empty-failure"
        })
        .expect("session id");
        kernel
            .create_session(SessionCreateOptions {
                session_id: session_id.clone(),
                cwd: temporary.path().to_path_buf(),
                parent_session_id: None,
            })
            .await
            .expect("create session");

        let result = kernel
            .run(
                RunRequest {
                    session_id: session_id.clone(),
                    input: "trigger stream failure".to_string(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
            .expect("failed attempt should settle");
        let assistant = result
            .messages
            .iter()
            .find(|message| {
                matches!(message.content, MessageContent::Assistant { .. })
            })
            .expect("assistant message");
        let MessageContent::Assistant { blocks, metadata } = &assistant.content
        else {
            unreachable!("assistant variant was selected above");
        };
        assert_eq!(
            blocks.first().and_then(ContentBlock::text),
            Some(partial.unwrap_or(""))
        );
        assert_eq!(metadata.stop_reason, StopReason::Error);
        assert_eq!(metadata.error.as_deref(), Some("socket closed"));
        assert!(assistant.timing.ended_at_ms >= assistant.timing.started_at_ms);
        assert!(matches!(
            result.turns.last().map(|turn| &turn.outcome),
            Some(protocol::TurnOutcome::Failed { message })
                if message == "socket closed"
        ));
        assert!(
            kernel
                .session_messages(&session_id)
                .expect("replay messages")
                .iter()
                .any(|message| message.identity.message_id
                    == assistant.identity.message_id)
        );
    }
}

/// Sibling tools complete concurrently but tool-result messages retain call order.
#[tokio::test]
async fn tool_batch_emits_completion_order_and_persists_source_order() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(2_000)));
    let slow_id = ToolCallId::try_from("slow").expect("slow id");
    let fast_id = ToolCallId::try_from("fast").expect("fast id");
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::from([
            ScriptedResponse::events(vec![
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: slow_id.clone(),
                    name: "delayed_text".to_string(),
                    arguments: serde_json::json!({
                        "delay_ms": 20,
                        "text": "slow"
                    }),
                }),
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: fast_id.clone(),
                    name: "delayed_text".to_string(),
                    arguments: serde_json::json!({
                        "delay_ms": 0,
                        "text": "fast"
                    }),
                }),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            ScriptedResponse::events(vec![
                ModelStreamEvent::TextDelta("done".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(DelayedToolFactory))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            temporary.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build kernel");
    let session_id = SessionId::try_from("session-tools").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            // Project-context discovery requires a real working directory.
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let sink = Arc::new(RecordingSink::default());

    let result = kernel
        .run(
            RunRequest {
                session_id,
                input: "run tools".to_string(),
            },
            sink.clone(),
        )
        .await
        .expect("run kernel");

    assert_eq!(result.turns.len(), 2);
    let persisted_tool_ids = result
        .messages
        .iter()
        .filter_map(|message| match &message.content {
            MessageContent::ToolResult { tool_call_id, .. } => {
                Some(tool_call_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(persisted_tool_ids, vec![slow_id.clone(), fast_id.clone()]);
    assert!(
        result
            .messages
            .iter()
            .filter(|message| {
                matches!(message.content, MessageContent::ToolResult { .. })
            })
            .all(|message| message.timing.started_at_ms
                < message.timing.ended_at_ms)
    );

    let completion_ids = sink
        .0
        .lock()
        .expect("sink lock")
        .iter()
        .filter_map(|event| match &event.payload {
            AgentEventPayload::ToolExecutionEnd { result, .. } => {
                Some(result.tool_call_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(completion_ids, vec![fast_id, slow_id]);
}

/// Policy-blocked tools persist a typed outcome that protocol clients can distinguish.
#[tokio::test]
async fn blocked_tool_result_preserves_policy_outcome() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(2_400)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::from([
            ScriptedResponse::events(vec![
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: ToolCallId::try_from("blocked-call")
                        .expect("tool id"),
                    name: "delayed_text".to_string(),
                    arguments: serde_json::json!({
                        "delay_ms": 0,
                        "text": "must not execute"
                    }),
                }),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            ScriptedResponse::events(vec![
                ModelStreamEvent::TextDelta("blocked".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(DelayedToolFactory))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            temporary.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            protocol::StaticExtensionRegistration::default(),
            vec![Arc::new(|| {
                Ok(Arc::new(BlockingToolExtension) as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-blocked-tool").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create session");

    let result = kernel
        .run(
            RunRequest {
                session_id,
                input: "run blocked tool".to_string(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("run kernel");
    let tool_message = result
        .messages
        .iter()
        .find(|message| {
            matches!(message.content, MessageContent::ToolResult { .. })
        })
        .expect("blocked tool result");
    let value = serde_json::to_value(tool_message).expect("serialize result");

    assert_eq!(value["content"]["details"]["type"], "blocked");
    assert_eq!(value["content"]["details"]["reason"], "policy denied");
    assert_eq!(value["content"]["details"]["terminate"], false);
}

/// Tool snapshots are emitted before the final result and retain correlation metadata.
#[tokio::test]
async fn tool_partial_update_precedes_final_result() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(2_500)));
    let tool_call_id = ToolCallId::try_from("stream-1").expect("tool id");
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::from([
            ScriptedResponse::events(vec![
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    name: "streaming".to_string(),
                    arguments: serde_json::json!({}),
                }),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            ScriptedResponse::events(vec![
                ModelStreamEvent::TextDelta("done".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
    });
    let extension_events = Arc::new(Mutex::new(Vec::new()));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(StreamingToolFactory))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            temporary.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            protocol::StaticExtensionRegistration::default(),
            vec![Arc::new({
                let extension_events = Arc::clone(&extension_events);
                move || {
                    Ok(Arc::new(RecordingExtension(Arc::clone(
                        &extension_events,
                    ))) as Arc<dyn ExtensionModule>)
                }
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-streaming-tool").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let sink = Arc::new(RecordingSink::default());

    kernel
        .run(
            RunRequest {
                session_id,
                input: "stream tool".to_string(),
            },
            sink.clone(),
        )
        .await
        .expect("run kernel");

    let events = sink.0.lock().expect("sink lock");
    let update_index = events
        .iter()
        .position(|event| {
            matches!(
                &event.payload,
                AgentEventPayload::ToolExecutionUpdate { result, .. }
                    if result.tool_call_id == tool_call_id
                        && result.blocks[0].text() == Some("partial")
            )
        })
        .expect("partial update");
    let end_index = events
        .iter()
        .position(|event| {
            matches!(
                &event.payload,
                AgentEventPayload::ToolExecutionEnd { result, .. }
                    if result.tool_call_id == tool_call_id
                        && result.blocks[0].text() == Some("final")
            )
        })
        .expect("final result");
    assert!(update_index < end_index);
    assert!(
        events[update_index].metadata.timestamp_ms
            <= events[end_index].metadata.timestamp_ms
    );
    assert!(
        extension_events
            .lock()
            .expect("extension lock")
            .contains(&"tool_execution_update")
    );
}

/// Synchronous tool updates coalesce to the latest snapshot before completion.
#[tokio::test]
async fn tool_updates_coalesce_to_the_latest_snapshot() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(2_750)));
    let tool_call_id = ToolCallId::try_from("burst-1").expect("tool id");
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::from([
            ScriptedResponse::events(vec![
                ModelStreamEvent::ToolCall(ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    name: "burst_updates".to_string(),
                    arguments: serde_json::json!({}),
                }),
                ScriptedModel::finished(StopReason::ToolUse),
            ]),
            ScriptedResponse::events(vec![
                ModelStreamEvent::TextDelta("done".to_string()),
                ScriptedModel::finished(StopReason::EndTurn),
            ]),
        ])),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BurstUpdateToolFactory))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            temporary.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-burst-tool").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let sink = Arc::new(RecordingSink::default());

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        kernel.run(
            RunRequest {
                session_id,
                input: "run burst tool".to_string(),
            },
            sink.clone(),
        ),
    )
    .await
    .expect("burst tool must settle")
    .expect("run kernel");

    let events = sink.0.lock().expect("sink lock");
    let updates = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match &event.payload {
            AgentEventPayload::ToolExecutionUpdate { result, .. }
                if result.tool_call_id == tool_call_id =>
            {
                Some((index, result))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let end_index = events
        .iter()
        .position(|event| {
            matches!(
                &event.payload,
                AgentEventPayload::ToolExecutionEnd { result, .. }
                    if result.tool_call_id == tool_call_id
            )
        })
        .expect("tool end");

    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].1.blocks[0].text(), Some("snapshot-1999"));
    assert!(updates[0].0 < end_index);
}

/// Session cancellation interrupts a pending provider stream and settles the Turn.
#[tokio::test]
async fn cancellation_settles_pending_turn_as_cancelled() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(3_000)));
    let model: Arc<dyn Model> = Arc::new(PendingModel);
    let kernel = Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(StaticModelFactory(model)))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                temporary.path(),
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                temporary.path().join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::default()))
            .clock(clock)
            .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
            .build()
            .build()
            .expect("build kernel"),
    );
    let session_id = SessionId::try_from("session-cancel").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            // Project-context discovery requires a real working directory.
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let running_kernel = Arc::clone(&kernel);
    let running_session_id = session_id.clone();
    let task = tokio::spawn(async move {
        running_kernel
            .run(
                RunRequest {
                    session_id: running_session_id,
                    input: "wait".to_string(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
    });
    tokio::task::yield_now().await;

    kernel
        .cancel_session(&session_id)
        .expect("cancel active session");
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("run should settle")
        .expect("run task")
        .expect("cancelled run result");

    assert!(matches!(
        result.turns.last().map(|turn| &turn.outcome),
        Some(protocol::TurnOutcome::Cancelled)
    ));
    let assistant = result
        .messages
        .iter()
        .find(|message| {
            matches!(message.content, MessageContent::Assistant { .. })
        })
        .expect("cancelled assistant message");
    let MessageContent::Assistant { metadata, .. } = &assistant.content else {
        unreachable!("assistant variant was selected above");
    };
    assert_eq!(metadata.stop_reason, StopReason::Cancelled);
    assert!(assistant.timing.ended_at_ms >= assistant.timing.started_at_ms);
    assert!(
        kernel
            .session_messages(&session_id)
            .expect("replay messages")
            .iter()
            .any(|message| message.identity.message_id
                == assistant.identity.message_id)
    );
}

/// Closing releases runtime state while persisted sessions remain listable and resumable.
#[tokio::test]
async fn persisted_session_can_be_listed_closed_and_resumed() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(4_000)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            temporary.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build kernel");
    let session_id = SessionId::try_from("session-resume").expect("session id");
    let cwd = temporary.path().join("workspace/resume");
    std::fs::create_dir_all(&cwd).expect("create project cwd");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");

    let sessions = kernel
        .list_sessions(Some(&cwd))
        .expect("list persisted sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, session_id);
    kernel
        .close_session(&session_id)
        .await
        .expect("close session");
    assert!(matches!(
        kernel.cancel_session(&session_id),
        Err(kernel::KernelError::SessionNotFound(_))
    ));
    let resumed_path = kernel
        .resume_session(session_id.clone(), cwd.clone())
        .await
        .expect("resume session");
    let repeated_path = kernel
        .resume_session(session_id.clone(), cwd)
        .await
        .expect("repeat resume session");
    assert_eq!(repeated_path, resumed_path);
    kernel
        .cancel_session(&session_id)
        .expect("resumed session is active");
}

/// Resume rejects an Assistant entry whose mandatory provider metadata is missing.
#[tokio::test]
async fn resume_rejects_assistant_without_metadata_as_invalid_session() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(5_000)));
    let store_factory =
        Arc::new(JsonlStoreFactory::new(temporary.path(), Arc::clone(&clock)));
    let session_id =
        SessionId::try_from("session-corrupt").expect("session id");
    let mut session = store_factory
        .create(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: temporary.path().to_path_buf(),
            parent_session_id: None,
        })
        .expect("create persisted session");
    session
        .append_entry(
            &LaneId::try_from("main").expect("main lane"),
            NewEntry {
                id: EntryId::try_from("entry-corrupt").expect("entry id"),
                kind: EntryKind::Message,
                payload: serde_json::json!({
                    "message_id": "message-corrupt",
                    "turn_id": "turn-corrupt",
                    "timestamp_ms": "5001",
                    "started_at_ms": "5001",
                    "ended_at_ms": "5001",
                    "content": {
                        "type": "assistant",
                        "blocks": []
                    }
                }),
            },
        )
        .expect("append corrupt assistant");
    drop(session);
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(store_factory)
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            temporary.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(Mutex::new(0))))
        .build()
        .build()
        .expect("build kernel");

    let error = kernel
        .resume_session(session_id, temporary.path().to_path_buf())
        .await
        .expect_err("missing Assistant metadata must fail resume");
    assert!(matches!(
        error,
        kernel::KernelError::Store(store::StoreError::InvalidSession(_))
    ));
}
