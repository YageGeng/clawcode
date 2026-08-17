//! Integration tests for unified Kernel Slash Command routing.

use std::fs;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use extension::{
    ExtensionCommandContext, ExtensionCommandHandler, ExtensionError,
    ExtensionModule, ExtensionRegistrar, StaticExtensionFactory,
};
use futures::{Stream, stream};
use kernel::{
    EventSink, Kernel, KernelFactory, Model, ModelError, ModelFactory,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, ContentBlock, ExtensionDescriptor,
    ExtensionId, ExtensionMessageDraft, ExtensionUserMessage, IdGenerator,
    IdKind, MessageContent, ModelFinal, ModelProfile, ModelRequest,
    ModelStreamEvent, ModelUsage, QueueKind, RunInput, RunRequest, SessionId,
    SlashCommandMessage, SlashCommandStatus, StopReason, TimestampMs,
};
use store::{Clock, JsonlStoreFactory, SessionCreateOptions};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

type TestModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// Immutable provider capabilities used by the local capture model.
static TEST_MODEL_PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("slash-command".to_string())
        .display_name("Slash Command fixture".to_string())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .build()
});

/// Supplies deterministic timestamps for persisted message assertions.
struct StepClock(AtomicU64);

impl Clock for StepClock {
    /// Advances the timestamp by one millisecond for each observation.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// Supplies readable identifiers without coupling expectations to Nanoid output.
struct SequentialIds(AtomicU64);

impl IdGenerator for SequentialIds {
    /// Returns one unique identifier with the requested protocol prefix.
    fn next(&self, kind: IdKind) -> String {
        let value = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        format!("{}-{value}", kind.prefix())
    }
}

/// Observable boundaries shared by the local Model and Extension command.
#[derive(typed_builder::TypedBuilder)]
struct CaptureState {
    #[builder(default)]
    model_requests: Mutex<Vec<ModelRequest>>,
    #[builder(default)]
    command_arguments: Mutex<Vec<String>>,
    #[builder(default)]
    block_model: std::sync::atomic::AtomicBool,
    #[builder(default)]
    model_started: tokio::sync::Notify,
    #[builder(default)]
    release_model: tokio::sync::Notify,
}

impl CaptureState {
    /// Arms the capture model so the next request remains active until released.
    fn block_next_model_request(&self) {
        self.block_model.store(true, Ordering::Release);
    }

    /// Waits until the capture model has entered its blocked request.
    async fn wait_for_model_request(&self) {
        self.model_started.notified().await;
    }

    /// Releases the currently blocked model request.
    fn release_model_request(&self) {
        self.block_model.store(false, Ordering::Release);
        self.release_model.notify_one();
    }
}

/// Captures complete Kernel model requests and returns one successful response.
struct CaptureModel(Arc<CaptureState>);

#[async_trait]
impl Model for CaptureModel {
    /// Returns deterministic model capabilities.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that the in-process fixture has no readiness dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Records the real request reaching the Model boundary before ending the Turn.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        if self.0.block_model.load(Ordering::Acquire) {
            self.0.model_started.notify_one();
            self.0.release_model.notified().await;
        }
        let is_compaction = request.tools.is_empty();
        self.0
            .model_requests
            .lock()
            .expect("model request lock")
            .push(request);
        let final_event = ModelStreamEvent::Finished(ModelFinal {
            stop_reason: StopReason::EndTurn,
            raw_stop_reason: None,
            usage: ModelUsage::builder()
                .input_tokens(10)
                .output_tokens(1)
                .cache_read_tokens(0)
                .cache_write_tokens(0)
                .total_tokens(11)
                .build(),
        });
        let events = if is_compaction {
            vec![
                Ok(ModelStreamEvent::TextDelta(
                    "Compacted summary".to_string(),
                )),
                Ok(final_event),
            ]
        } else {
            vec![Ok(final_event)]
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

/// Returns the same capture model for every Session in this test fixture.
struct CaptureModelFactory(Arc<CaptureState>);

impl ModelFactory for CaptureModelFactory {
    /// Creates one model handle sharing the fixture's observable state.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(CaptureModel(Arc::clone(&self.0))))
    }
}

/// Direct Extension command that records the exact unparsed argument tail.
struct CaptureCommand(Arc<CaptureState>);

#[async_trait]
impl ExtensionCommandHandler for CaptureCommand {
    /// Records invocation arguments without starting Agent work.
    async fn handle(
        &self,
        arguments: &str,
        _parameters: &serde_json::Value,
        _context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        self.0
            .command_arguments
            .lock()
            .expect("command argument lock")
            .push(arguments.to_string());
        Ok(())
    }
}

/// Extension command that proves command handlers can safely start a nested Agent Run.
struct ForwardCommand;

#[async_trait]
impl ExtensionCommandHandler for ForwardCommand {
    /// Sends one model-facing message and then one visible extension message.
    async fn handle(
        &self,
        _arguments: &str,
        _parameters: &serde_json::Value,
        context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        context
            .event
            .send_user_message(ExtensionUserMessage {
                text: "from Slash Extension".to_string(),
                delivery: QueueKind::FollowUp,
                parent_entry_id: None,
            })
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        context
            .event
            .send_extension_message(
                ExtensionMessageDraft::builder()
                    .extension_id(
                        ExtensionId::try_from("sample").expect("ExtensionId"),
                    )
                    .custom_type("forwarded".to_string())
                    .blocks(vec![ContentBlock::Text {
                        text: "forward complete".to_string(),
                    }])
                    .display(true)
                    .include_in_context(false)
                    .build(),
            )
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        Ok(())
    }
}

/// Extension command that returns an intentionally sensitive internal diagnostic.
struct FailingCommand;

#[async_trait]
impl ExtensionCommandHandler for FailingCommand {
    /// Returns an internal failure that must never reach persisted client output.
    async fn handle(
        &self,
        _arguments: &str,
        _parameters: &serde_json::Value,
        _context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        Err(ExtensionError::Handler(
            "authorization=secret-value".to_string(),
        ))
    }
}

/// Registers one direct command under a stable Extension identity.
struct CommandModule(Arc<CaptureState>);

impl ExtensionModule for CommandModule {
    /// Returns the stable Extension descriptor used by qualified-name assertions.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("sample").expect("ExtensionId"),
            name: "Sample".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the direct command exercised by the routing tests.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.register_command(
            protocol::ExtensionCommandDefinition {
                name: "handled".to_string(),
                description: Some("Handle without an Agent Turn".to_string()),
                argument_hint: Some("[arguments]".to_string()),
            },
            CaptureCommand(Arc::clone(&self.0)),
        )?;
        registrar.register_command(
            protocol::ExtensionCommandDefinition {
                name: "ambiguous".to_string(),
                description: Some("Ambiguous short command".to_string()),
                argument_hint: None,
            },
            CaptureCommand(Arc::clone(&self.0)),
        )?;
        registrar.register_command(
            protocol::ExtensionCommandDefinition {
                name: "forward".to_string(),
                description: Some("Forward one user message".to_string()),
                argument_hint: None,
            },
            ForwardCommand,
        )?;
        registrar.register_command(
            protocol::ExtensionCommandDefinition {
                name: "fails".to_string(),
                description: Some("Return an internal failure".to_string()),
                argument_hint: None,
            },
            FailingCommand,
        )
    }
}

/// Second owner used to make one Extension short name ambiguous.
struct AmbiguousModule(Arc<CaptureState>);

impl ExtensionModule for AmbiguousModule {
    /// Returns the second stable Extension identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("other").expect("ExtensionId"),
            name: "Other".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the colliding command under the second qualified name.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.register_command(
            protocol::ExtensionCommandDefinition {
                name: "ambiguous".to_string(),
                description: Some("Second ambiguous command".to_string()),
                argument_hint: None,
            },
            CaptureCommand(Arc::clone(&self.0)),
        )
    }
}

/// Captures runtime events in exact delivery order.
#[derive(Default)]
struct RecordingSink(Mutex<Vec<AgentEvent>>);

#[async_trait]
impl EventSink for RecordingSink {
    /// Retains one complete event without altering metadata.
    async fn emit(&self, event: AgentEvent) -> Result<(), kernel::SinkError> {
        self.0.lock().expect("event lock").push(event);
        Ok(())
    }
}

/// Kernel resources needed by Slash Command integration tests.
struct Fixture {
    kernel: Arc<Kernel>,
    session_id: SessionId,
    state: Arc<CaptureState>,
}

/// Builds one real Kernel Session with isolated Store, Prompt, Tool, and Extension factories.
async fn fixture() -> (tempfile::TempDir, Fixture) {
    let root = tempfile::tempdir().expect("fixture root");
    let config_root = root.path().join("config");
    let prompts = config_root.join("prompts");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&prompts).expect("Prompt directory");
    fs::create_dir_all(&cwd).expect("Session cwd");
    fs::write(
        prompts.join("review.md"),
        "---\ndescription: Review code\nargument-hint: <path>\n---\nREVIEW TEMPLATE: $ARGUMENTS",
    )
    .expect("Prompt Template");
    let state = Arc::new(CaptureState::builder().build());
    let extension_state = Arc::clone(&state);
    let ambiguous_state = Arc::clone(&state);
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(10_000)));
    let kernel = Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(CaptureModelFactory(Arc::clone(&state))))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root.path(),
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                config_root,
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::new(
                protocol::StaticExtensionRegistration::default(),
                vec![
                    Arc::new(move || {
                        Ok(
                            Arc::new(CommandModule(Arc::clone(
                                &extension_state,
                            )))
                                as Arc<dyn ExtensionModule>,
                        )
                    }),
                    Arc::new(move || {
                        Ok(Arc::new(AmbiguousModule(Arc::clone(
                            &ambiguous_state,
                        )))
                            as Arc<dyn ExtensionModule>)
                    }),
                ],
            )))
            .clock(clock)
            .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
            .compaction_policy(
                protocol::CompactionPolicy::builder()
                    .enabled(true)
                    .reserve_tokens(16_384)
                    .keep_recent_tokens(0)
                    .build(),
            )
            .build()
            .build()
            .expect("Kernel"),
    );
    let session_id =
        SessionId::try_from("session-slash-command").expect("SessionId");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("Session");
    (
        root,
        Fixture {
            kernel,
            session_id,
            state,
        },
    )
}

/// A handled Extension command persists its invocation and never reaches the Model.
#[tokio::test]
async fn handled_extension_command_persists_one_correlated_command_turn() {
    let (_root, fixture) = fixture().await;
    let sink = Arc::new(RecordingSink::default());

    let result = fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: RunInput::Text("/sample:handled alpha beta".to_string()),
            },
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .await
        .expect("handled command");

    assert!(result.turns.is_empty());
    assert!(
        fixture
            .state
            .model_requests
            .lock()
            .expect("model request lock")
            .is_empty()
    );
    assert_eq!(
        *fixture
            .state
            .command_arguments
            .lock()
            .expect("command argument lock"),
        vec!["alpha beta"]
    );
    let invocation = result
        .messages
        .iter()
        .find(|message| {
            matches!(
                &message.content,
                MessageContent::SlashCommand {
                    message: SlashCommandMessage::Invocation { .. }
                }
            )
        })
        .expect("persisted command invocation");
    let events = sink.0.lock().expect("event lock");
    let start = events
        .iter()
        .find(|event| {
            matches!(event.payload, AgentEventPayload::SlashCommandStart { .. })
        })
        .expect("Slash Command start");
    let end = events
        .iter()
        .find(|event| {
            matches!(event.payload, AgentEventPayload::SlashCommandEnd { .. })
        })
        .expect("Slash Command end");
    assert_eq!(invocation.identity.turn_id, start.metadata.turn_id);
    assert_eq!(start.metadata.turn_id, end.metadata.turn_id);
}

/// Prompt Template replay retains the original invocation and frozen model expansion.
#[tokio::test]
async fn template_replay_keeps_original_and_model_expansion() {
    let (_root, fixture) = fixture().await;

    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: RunInput::Text("/review src/lib.rs".to_string()),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("Template run");

    let transcript = fixture
        .kernel
        .session_transcript(&fixture.session_id)
        .expect("transcript");
    let expansion = transcript
        .iter()
        .find_map(|message| match &message.content {
            MessageContent::ExpandedUser { expansion } => Some(expansion),
            _ => None,
        })
        .expect("Expanded User message");
    assert_eq!(expansion.invocation.original, "/review src/lib.rs");
    assert_eq!(
        expansion.model_blocks[0].text(),
        Some("REVIEW TEMPLATE: src/lib.rs")
    );
    let requests = fixture
        .state
        .model_requests
        .lock()
        .expect("model request lock");
    assert!(requests[0].messages.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::ExpandedUser { expansion }
                if expansion.model_blocks.iter().any(|block| {
                    block.text() == Some("REVIEW TEMPLATE: src/lib.rs")
                })
        )
    }));
}

/// Composite input beginning with a slash bypasses command dispatch and reaches the Model.
#[tokio::test]
async fn composite_slash_input_is_an_ordinary_user_prompt() {
    let (_root, fixture) = fixture().await;

    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id,
                input: RunInput::Composite(
                    "/session\n\n[spec](file:///workspace/spec.md)".to_string(),
                ),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("ordinary composite prompt");

    let requests = fixture
        .state
        .model_requests
        .lock()
        .expect("model request lock");
    assert!(requests[0].messages.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::User { blocks }
                if blocks == &vec![ContentBlock::Text {
                    text: "/session\n\n[spec](file:///workspace/spec.md)".to_string(),
                }]
        )
    }));
}

/// The name Builtin updates persisted Session metadata and reports it without inference.
#[tokio::test]
async fn name_command_sets_and_queries_the_persisted_title() {
    let (_root, fixture) = fixture().await;

    let set = fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/name Slash E2E".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("set Session name");
    assert!(set.turns.is_empty());
    assert_eq!(
        fixture
            .kernel
            .session_tree(&fixture.session_id)
            .expect("Session tree")
            .name
            .as_deref(),
        Some("Slash E2E")
    );

    let query = fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/name".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("query Session name");
    assert!(query.messages.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::SlashCommand {
                message: SlashCommandMessage::Output(output)
            } if output.blocks.iter().any(|block| {
                block.text().is_some_and(|text| text.contains("Slash E2E"))
            })
        )
    }));
    assert!(
        fixture
            .state
            .model_requests
            .lock()
            .expect("model request lock")
            .is_empty()
    );
}

/// The session Builtin reports persisted statistics and rejects unexpected arguments.
#[tokio::test]
async fn session_command_reports_usage_without_calling_the_model() {
    let (_root, fixture) = fixture().await;

    let result = fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/session".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("Session statistics");
    assert!(result.messages.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::SlashCommand {
                message: SlashCommandMessage::Output(output)
            } if output.status == protocol::SlashCommandStatus::Succeeded
                && output.blocks.iter().any(|block| {
                    block.text().is_some_and(|text| {
                        text.contains("Session ID")
                            && text.contains("session-slash-command")
                    })
                })
        )
    }));

    let rejected = fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id,
                input: "/session unexpected".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("rejected Session arguments");
    assert!(rejected.messages.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::SlashCommand {
                message: SlashCommandMessage::Output(output)
            } if output.status == protocol::SlashCommandStatus::Failed
        )
    }));
    assert!(
        fixture
            .state
            .model_requests
            .lock()
            .expect("model request lock")
            .is_empty()
    );
}

/// The compact Builtin supplies custom instructions and preserves one command Turn.
#[tokio::test]
async fn compact_command_uses_custom_instruction_and_one_turn() {
    let (_root, fixture) = fixture().await;
    let sink = Arc::new(RecordingSink::default());
    fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "context before compact".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("prepare model context");

    let result = fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/compact Preserve API decisions".into(),
            },
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .await
        .expect("compact command");

    let requests = fixture
        .state
        .model_requests
        .lock()
        .expect("model request lock");
    let request_text = requests
        .iter()
        .flat_map(|request| &request.messages)
        .filter_map(|message| message.content.text_content())
        .collect::<Vec<_>>();
    assert!(
        request_text
            .iter()
            .any(|text| text.contains("Preserve API decisions")),
        "captured request text: {request_text:?}"
    );
    let command_turn = result.messages[0].identity.turn_id.clone();
    let events = sink.0.lock().expect("event lock");
    assert!(
        events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    AgentEventPayload::SlashCommandStart { .. }
                        | AgentEventPayload::SlashCommandEnd { .. }
                        | AgentEventPayload::CompactionStart { .. }
                        | AgentEventPayload::CompactionEnd { .. }
                )
            })
            .all(|event| event.metadata.turn_id == command_turn)
    );
    assert!(result.messages.iter().all(|message| !matches!(
        &message.content,
        MessageContent::SlashCommand {
            message: SlashCommandMessage::Output(_)
        }
    )));
}

/// Busy direct commands fail immediately instead of waiting behind the active Agent Run.
#[tokio::test]
async fn direct_command_returns_busy_without_waiting_for_the_active_run() {
    let (_root, fixture) = fixture().await;
    fixture.state.block_next_model_request();
    let running_kernel = Arc::clone(&fixture.kernel);
    let running_session = fixture.session_id.clone();
    let running = tokio::spawn(async move {
        running_kernel
            .run(
                RunRequest {
                    session_id: running_session,
                    input: "keep running".into(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
    });
    fixture.state.wait_for_model_request().await;

    let rejected = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        fixture.kernel.run(
            RunRequest {
                session_id: fixture.session_id.clone(),
                input: "/session".into(),
            },
            Arc::new(RecordingSink::default()),
        ),
    )
    .await
    .expect("busy command must not wait")
    .expect("busy command result");

    assert!(rejected.messages.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::SlashCommand {
                message: SlashCommandMessage::Output(output)
            } if output.status == SlashCommandStatus::Failed
                && output.blocks.iter().any(|block| {
                    block.text().is_some_and(|text| text.contains("busy"))
                })
        )
    }));
    fixture.state.release_model_request();
    running
        .await
        .expect("running task")
        .expect("running result");
}

/// Slash Extension handlers can start Agent work and still stream later extension messages.
#[tokio::test]
async fn extension_command_reentry_keeps_its_live_event_sink() {
    let (_root, fixture) = fixture().await;
    let sink = Arc::new(RecordingSink::default());

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        fixture.kernel.run(
            RunRequest {
                session_id: fixture.session_id,
                input: "/sample:forward".into(),
            },
            Arc::clone(&sink) as Arc<dyn EventSink>,
        ),
    )
    .await
    .expect("Extension command must not deadlock")
    .expect("Extension command result");

    assert!(
        fixture
            .state
            .model_requests
            .lock()
            .expect("model request lock")
            .iter()
            .any(|request| request.messages.iter().any(|message| {
                message.content.text_content().as_deref()
                    == Some("from Slash Extension")
            }))
    );
    assert!(sink.0.lock().expect("event lock").iter().any(|event| {
        matches!(
            &event.payload,
            AgentEventPayload::MessageEnd { message }
                if matches!(
                    &message.content,
                    MessageContent::Extension { extension }
                        if extension.custom_type == "forwarded"
                )
        )
    }));
}

/// Extension failures persist a stable message without leaking handler diagnostics.
#[tokio::test]
async fn extension_command_failure_is_sanitized_for_clients() {
    let (_root, fixture) = fixture().await;

    let result = fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id,
                input: "/sample:fails".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("failed Extension command result");
    let output = result
        .messages
        .iter()
        .find_map(|message| match &message.content {
            MessageContent::SlashCommand {
                message: SlashCommandMessage::Output(output),
            } => output.blocks.iter().find_map(ContentBlock::text),
            _ => None,
        })
        .expect("failed command output");

    assert_eq!(output, "/sample:fails failed");
    assert!(!output.contains("secret-value"));
}

/// Ambiguous short commands produce a replayable failure with qualified alternatives.
#[tokio::test]
async fn ambiguous_extension_command_lists_qualified_candidates() {
    let (_root, fixture) = fixture().await;

    let result = fixture
        .kernel
        .run(
            RunRequest {
                session_id: fixture.session_id,
                input: "/ambiguous".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("ambiguous command result");
    let output = result
        .messages
        .iter()
        .find_map(|message| match &message.content {
            MessageContent::SlashCommand {
                message: SlashCommandMessage::Output(output),
            } => output.blocks.iter().find_map(ContentBlock::text),
            _ => None,
        })
        .expect("ambiguous command output");

    assert!(output.contains("/sample:ambiguous"));
    assert!(output.contains("/other:ambiguous"));
}
