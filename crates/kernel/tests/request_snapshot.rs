//! Every agent-loop attempt records the request it sent, and each request
//! content is written to the Session log once, before the first attempt that
//! sends it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use extension::StaticExtensionFactory;
use futures::{Stream, stream};
use kernel::{
    EventSink, Kernel, KernelFactory, Model, ModelError, ModelFactory,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, AgentMessage, AssistantMetadata,
    IdGenerator, IdKind, MessageContent, ModelFailure, ModelFinal,
    ModelProfile, ModelRequest, ModelRetryDisposition, ModelStreamEvent,
    RetryPolicy, RunRequest, SessionId, StopReason, TimestampMs, ToolCall,
    ToolCallId,
};
use store::{
    Clock, EntryKind, JsonlStoreFactory, RecordKind, SessionCreateOptions,
    StoreFactory,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

type TestModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// The profile of the scripted model.
static PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("snapshot".to_string())
        .display_name("Snapshot fixture".to_string())
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
    /// The request fails before a stream exists.
    Failure(ModelFailure),
    /// The request streams these events.
    Events(Vec<ModelStreamEvent>),
}

impl Response {
    /// A complete text answer.
    fn text(text: &str) -> Self {
        Self::Events(vec![
            ModelStreamEvent::TextDelta(text.to_string()),
            Self::finished(StopReason::EndTurn),
        ])
    }

    /// One call of the built-in `read` tool.
    fn read(path: &str) -> Self {
        Self::Events(vec![
            ModelStreamEvent::ToolCall(
                ToolCall::builder()
                    .tool_call_id(ToolCallId::try_from("call-1").expect("id"))
                    .name("read".to_string())
                    .arguments(serde_json::json!({ "path": path }))
                    .build(),
            ),
            Self::finished(StopReason::ToolUse),
        ])
    }

    /// A retryable request failure.
    fn transient() -> Self {
        Self::Failure(
            ModelFailure::builder()
                .summary("service unavailable".to_string())
                .status(Some(503))
                .retry_disposition(ModelRetryDisposition::Retryable)
                .build(),
        )
    }

    /// Terminal metadata with small usage.
    fn finished(stop_reason: StopReason) -> ModelStreamEvent {
        ModelStreamEvent::Finished(
            ModelFinal::builder()
                .stop_reason(stop_reason)
                .usage(protocol::ModelUsage::from(
                    protocol::Usage::new().input_tokens(20).output_tokens(2),
                ))
                .build(),
        )
    }
}

/// Records every request and answers from a script.
struct CaptureModel {
    responses: Mutex<VecDeque<Response>>,
    requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl Model for CaptureModel {
    /// Returns the fixture profile.
    fn profile(&self) -> &ModelProfile {
        &PROFILE
    }

    /// Needs no provider.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Records the request and returns the next scripted response.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        self.requests.lock().expect("requests").push(request);
        match self
            .responses
            .lock()
            .expect("responses")
            .pop_front()
            .expect("scripted response")
        {
            Response::Failure(failure) => Err(ModelError::Request(failure)),
            Response::Events(events) => {
                Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
            }
        }
    }
}

/// Hands out the shared capture model.
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
    /// Waits until retry `attempt` is scheduled and returns its delay.
    async fn retry_delay(&self, attempt: u32) -> u64 {
        loop {
            let delay =
                self.events
                    .lock()
                    .expect("events")
                    .iter()
                    .find_map(|event| match &event.payload {
                        AgentEventPayload::RetryScheduled {
                            attempt: scheduled,
                            delay_ms,
                            ..
                        } if *scheduled == attempt => Some(*delay_ms),
                        _ => None,
                    });
            if let Some(delay) = delay {
                return delay;
            }
            self.changed.notified().await;
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

/// The persisted log of the fixture Session, in sequence order.
struct Log {
    /// Assistant messages with their entry sequences.
    assistants: Vec<(u64, AssistantMetadata)>,
    /// Request content records: sequence and payload.
    contents: Vec<(u64, serde_json::Value)>,
}

/// One kernel with a scripted model and one Session.
#[derive(typed_builder::TypedBuilder)]
struct Fixture {
    kernel: Arc<Kernel>,
    model: Arc<CaptureModel>,
    clock: Arc<dyn Clock>,
    session_id: SessionId,
    cwd: PathBuf,
    path: PathBuf,
    root: tempfile::TempDir,
}

impl Fixture {
    /// Builds the kernel without extensions and creates the Session.
    async fn new(responses: Vec<Response>) -> Self {
        Self::with_extensions(responses, StaticExtensionFactory::default())
            .await
    }

    /// Builds the kernel with `extensions` and creates the Session.
    async fn with_extensions(
        responses: Vec<Response>,
        extensions: StaticExtensionFactory,
    ) -> Self {
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("workspace");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::write(cwd.join("notes.txt"), "notes").expect("file");
        let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(10_000)));
        let model = Arc::new(CaptureModel {
            responses: Mutex::new(VecDeque::from(responses)),
            requests: Mutex::new(Vec::new()),
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
                .extension_factory(Arc::new(extensions))
                .clock(Arc::clone(&clock))
                .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
                .retry_policy(
                    RetryPolicy::builder()
                        .enabled(true)
                        .max_retries(1)
                        .base_delay_ms(1_000)
                        .max_retry_delay_ms(1_000)
                        .build(),
                )
                .build()
                .build()
                .expect("kernel"),
        );
        let session_id = SessionId::try_from("session-snapshot").expect("id");
        let path = kernel
            .create_session(
                SessionCreateOptions::builder()
                    .session_id(session_id.clone())
                    .cwd(cwd.clone())
                    .build(),
            )
            .await
            .expect("session");
        Self::builder()
            .kernel(kernel)
            .model(model)
            .clock(clock)
            .session_id(session_id)
            .cwd(cwd)
            .path(path)
            .root(root)
            .build()
    }

    /// Runs one prompt to completion.
    async fn run(&self, sink: Arc<RecordingSink>) {
        self.kernel
            .run(
                RunRequest {
                    session_id: self.session_id.clone(),
                    input: "hello".into(),
                },
                sink,
            )
            .await
            .expect("run");
    }

    /// Closes the Session and reads its log back from disk.
    async fn log(&self) -> Log {
        self.kernel
            .close_session(&self.session_id)
            .await
            .expect("close");
        let store =
            JsonlStoreFactory::new(self.root.path(), Arc::clone(&self.clock))
                .open(&self.path)
                .expect("open log");
        let assistants = store
            .entries()
            .into_iter()
            .filter(|entry| entry.kind == EntryKind::Message)
            .filter_map(|entry| {
                let message: AgentMessage = serde_json::from_value(
                    serde_json::Value::Object(entry.payload),
                )
                .expect("message");
                match message.content {
                    MessageContent::Assistant { metadata, .. } => {
                        Some((entry.sequence.get(), metadata))
                    }
                    _ => None,
                }
            })
            .collect();
        let contents = store
            .records()
            .into_iter()
            .filter(|record| record.kind == RecordKind::RequestContent)
            .map(|record| {
                (
                    record.sequence.get(),
                    serde_json::Value::Object(record.payload),
                )
            })
            .collect();
        Log {
            assistants,
            contents,
        }
    }

    /// The requests the model received, captured as the kernel would.
    fn captured(&self) -> Vec<protocol::RequestCapture> {
        self.model
            .requests
            .lock()
            .expect("requests")
            .iter()
            .map(|request| request.capture(&PROFILE).expect("capture"))
            .collect()
    }
}

/// Every attempt carries the request it sent.
#[tokio::test]
async fn every_attempt_carries_the_request_it_sent() {
    let fixture =
        Fixture::new(vec![Response::read("notes.txt"), Response::text("done")])
            .await;
    fixture.run(Arc::default()).await;
    let captured = fixture.captured();
    let log = fixture.log().await;

    assert_eq!(log.assistants.len(), 2);
    assert_eq!(captured.len(), 2);
    for ((_, metadata), capture) in log.assistants.iter().zip(&captured) {
        let request = metadata.request.as_ref().expect("snapshot");
        assert_eq!(request.system_prompt, capture.snapshot.system_prompt);
        assert_eq!(request.tools, capture.snapshot.tools);
        assert_eq!(**request, capture.snapshot);
    }
}

/// Request contents are written once before the assistant.
#[tokio::test]
async fn request_contents_are_written_once_before_the_assistant() {
    let fixture =
        Fixture::new(vec![Response::read("notes.txt"), Response::text("done")])
            .await;
    fixture.run(Arc::default()).await;
    let log = fixture.log().await;

    assert_eq!(log.contents.len(), 2);
    let first_assistant = log.assistants[0].0;
    assert!(
        log.contents
            .iter()
            .all(|(sequence, _)| *sequence < first_assistant)
    );
    let kinds: Vec<_> = log
        .contents
        .iter()
        .map(|(_, payload)| payload["requestContent"]["kind"].clone())
        .collect();
    assert_eq!(kinds, ["system_prompt", "tools"]);
}

/// Reopened session does not rewrite contents.
#[tokio::test]
async fn reopened_session_does_not_rewrite_contents() {
    let fixture =
        Fixture::new(vec![Response::text("one"), Response::text("two")]).await;
    fixture.run(Arc::default()).await;
    fixture
        .kernel
        .close_session(&fixture.session_id)
        .await
        .expect("close");
    fixture
        .kernel
        .resume_session(fixture.session_id.clone(), fixture.cwd.clone())
        .await
        .expect("resume");
    fixture.run(Arc::default()).await;
    let log = fixture.log().await;

    assert_eq!(log.assistants.len(), 2);
    assert_eq!(log.contents.len(), 2);
}

/// Retry attempts each carry a snapshot.
#[tokio::test(start_paused = true)]
async fn retry_attempts_each_carry_a_snapshot() {
    let fixture =
        Fixture::new(vec![Response::transient(), Response::text("ok")]).await;
    let sink = Arc::new(RecordingSink::default());
    let run = {
        let kernel = Arc::clone(&fixture.kernel);
        let session_id = fixture.session_id.clone();
        let sink = Arc::clone(&sink);
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
    };
    let delay = sink.retry_delay(1).await;
    tokio::time::advance(Duration::from_millis(delay)).await;
    run.await.expect("join").expect("run");
    let log = fixture.log().await;

    assert_eq!(log.assistants.len(), 2);
    assert!(log.assistants[0].1.error.is_some());
    assert!(
        log.assistants
            .iter()
            .all(|(_, metadata)| metadata.request.is_some())
    );
}

/// The system prompt the rewriting extension sends instead.
const REWRITTEN_PROMPT: &str = "rewritten by an extension";

/// Replaces the system prompt of every model request.
struct RewritePrompt;

#[async_trait]
impl extension::ExtensionHandler<extension::ContextPoint> for RewritePrompt {
    /// Swaps the system message's text for [`REWRITTEN_PROMPT`].
    async fn handle(
        &self,
        event: &protocol::ContextEvent,
        _context: &extension::ExtensionContext,
    ) -> Result<protocol::ContextResult, extension::ExtensionError> {
        let messages = event
            .request
            .messages
            .iter()
            .cloned()
            .map(|mut message| {
                if let MessageContent::System { blocks } = &mut message.content
                {
                    *blocks = vec![protocol::ContentBlock::Text {
                        text: REWRITTEN_PROMPT.into(),
                    }];
                }
                message
            })
            .collect();
        Ok(protocol::ContextResult {
            messages: Some(messages),
        })
    }
}

/// The module that registers [`RewritePrompt`].
struct RewriteModule;

impl extension::ExtensionModule for RewriteModule {
    /// Names the test module.
    fn descriptor(&self) -> protocol::ExtensionDescriptor {
        protocol::ExtensionDescriptor {
            id: protocol::ExtensionId::try_from("rewrite").expect("id"),
            name: "rewrite".into(),
            version: "1".into(),
        }
    }

    /// Registers the prompt rewrite on the context point.
    fn register(
        &self,
        registrar: &mut extension::ExtensionRegistrar,
    ) -> Result<(), extension::ExtensionError> {
        registrar.on::<extension::ContextPoint, _>(RewritePrompt)
    }
}

/// The snapshot is taken from the request as extensions left it: its system
/// prompt is the rewritten one, the one the model received and the log holds.
#[tokio::test]
async fn the_snapshot_follows_extension_rewrites() {
    let extensions = StaticExtensionFactory::new(
        protocol::StaticExtensionRegistration::default(),
        vec![Arc::new(|| {
            Ok(Arc::new(RewriteModule) as Arc<dyn extension::ExtensionModule>)
        })],
    );
    let fixture =
        Fixture::with_extensions(vec![Response::text("done")], extensions)
            .await;
    fixture.run(Arc::default()).await;
    let log = fixture.log().await;

    let rewritten = protocol::ContentHash::of(REWRITTEN_PROMPT.as_bytes());
    let request = log.assistants[0].1.request.as_ref().expect("snapshot");
    assert_eq!(request.system_prompt, rewritten);
    assert_eq!(fixture.captured()[0].snapshot.system_prompt, rewritten);
    assert!(log.contents.iter().any(|(_, record)| {
        record.pointer("/requestContent/content")
            == Some(&serde_json::json!(REWRITTEN_PROMPT))
    }));
}
