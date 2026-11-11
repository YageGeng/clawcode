use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use extension::StaticExtensionFactory;
use kernel::{
    EventSink, Kernel, KernelFactory, Model, ModelError, ModelFactory,
    ModelStream, SessionEventSinks, SinkError, WhenBusy,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, ContentBlock, IdGenerator, IdKind,
    MessageContent, ModelFinal, ModelProfile, ModelRequest, ModelStreamEvent,
    SessionId, SessionSummary, StopReason, TimestampMs,
};
use store::{Clock, JsonlStoreFactory, SessionCreateOptions};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

static PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".into())
        .model_id("primary".into())
        .display_name("Routed".into())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .build()
});

pub enum Route {
    Events(Vec<ModelStreamEvent>),
    Wait(Arc<Notify>, Vec<ModelStreamEvent>),
    Hang,
    /// Fails the request before any stream exists.
    // Only the protocol_alignment target, which shares this fixture, fails requests.
    #[allow(dead_code)]
    Fail(fn() -> ModelError),
}

impl Route {
    /// Builds a complete textual answer without provider I/O.
    pub fn answer(text: &str) -> Self {
        Self::Events(vec![
            ModelStreamEvent::TextDelta(text.into()),
            finished(StopReason::EndTurn),
        ])
    }
}

/// Builds one terminal provider event with deterministic usage.
pub fn finished(stop_reason: StopReason) -> ModelStreamEvent {
    ModelStreamEvent::Finished(
        ModelFinal::builder()
            .stop_reason(stop_reason)
            .raw_stop_reason(None)
            .usage(protocol::ModelUsage::from(
                protocol::Usage::new()
                    .input_tokens(0)
                    .output_tokens(0)
                    .cached_input_tokens(0)
                    .cache_creation_input_tokens(0)
                    .total_tokens(0),
            ))
            .build(),
    )
}

/// Reads visible text without relying on a provider-specific message shape.
pub fn text(content: &MessageContent) -> String {
    match content {
        MessageContent::User { blocks }
        | MessageContent::Assistant { blocks, .. } => blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        MessageContent::Extension { extension } => extension
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

type Router = dyn Fn(&ModelRequest) -> Route + Send + Sync;

pub struct RoutedModel {
    profile: ModelProfile,
    route: Box<Router>,
    pub requests: Mutex<Vec<ModelRequest>>,
    /// Makes provider readiness fail so tests can end a Run before insertion.
    pub preflight_failure: Mutex<Option<fn() -> ModelError>>,
    /// Makes every model of the catalog report that this method needs a sign-in.
    pub sign_in_required: Arc<Mutex<Option<String>>>,
    /// Holds the sign-in check open so a test can act while it runs.
    pub sign_in_wait: Mutex<Option<Arc<Notify>>>,
}

impl RoutedModel {
    /// Counts the provider requests the Kernel actually sent.
    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("requests").len()
    }

    /// Returns the last message text from each actual Kernel provider request.
    pub fn last_texts(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("requests")
            .iter()
            .map(|r| {
                r.messages
                    .last()
                    .map_or_else(String::new, |m| text(&m.content))
            })
            .collect()
    }
}

#[async_trait]
impl Model for RoutedModel {
    /// Exposes stable capabilities for request building.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Avoids external provider checks unless a test scripts a readiness failure.
    async fn preflight(&self) -> Result<(), ModelError> {
        match *self.preflight_failure.lock().expect("preflight") {
            Some(failure) => Err(failure()),
            None => Ok(()),
        }
    }

    /// Reports the scripted sign-in state after any scripted wait.
    async fn ensure_signed_in(&self) -> Result<(), ModelError> {
        let wait = self.sign_in_wait.lock().expect("sign-in wait").clone();
        if let Some(wait) = wait {
            wait.notified().await;
        }
        match self.sign_in_required.lock().expect("sign-in").clone() {
            Some(method_id) => Err(ModelError::AuthRequired { method_id }),
            None => Ok(()),
        }
    }

    /// Controls only the model boundary; persistence, tools, and lifecycle remain real.
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        let route = (self.route)(&request);
        self.requests.lock().expect("requests").push(request);
        let events = match route {
            Route::Events(events) => events,
            Route::Wait(notify, events) => {
                tokio::select! { () = notify.notified() => {}, () = cancellation.cancelled() => return Err(ModelError::Cancelled) }
                events
            }
            Route::Hang => {
                cancellation.cancelled().await;
                return Err(ModelError::Cancelled);
            }
            Route::Fail(failure) => return Err(failure()),
        };
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

struct StaticModelFactory(Arc<RoutedModel>);
impl ModelFactory for StaticModelFactory {
    /// Shares the deterministic router across independently owned Sessions.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0) as Arc<dyn Model>)
    }

    /// Exposes a second profile so owned-session setting copies are observable.
    fn create_catalog(
        &self,
        _registration: &protocol::StaticExtensionRegistration,
    ) -> Result<kernel::ModelCatalog, ModelError> {
        let mut profile = PROFILE.clone();
        profile.model_id = "secondary".into();
        kernel::ModelCatalog::new(
            "fixture",
            "primary",
            vec![
                self.create()?,
                Arc::new(RoutedModel {
                    profile,
                    route: Box::new(|_| Route::answer("secondary answer")),
                    requests: Mutex::new(Vec::new()),
                    preflight_failure: Mutex::new(None),
                    sign_in_required: Arc::clone(&self.0.sign_in_required),
                    sign_in_wait: Mutex::new(None),
                }),
            ],
        )
    }
}

#[derive(Default)]
pub struct RecordingSinks(Arc<Mutex<Vec<(SessionId, AgentEvent)>>>);

struct RecordingSink {
    session: SessionId,
    events: Arc<Mutex<Vec<(SessionId, AgentEvent)>>>,
}

impl RecordingSinks {
    /// Returns every payload emitted for one Session in emission order.
    // Only the protocol_alignment target, which shares this fixture, reads payloads.
    #[allow(dead_code)]
    pub fn payloads(&self, session: &SessionId) -> Vec<AgentEventPayload> {
        self.0
            .lock()
            .expect("events")
            .iter()
            .filter(|(id, _event)| id == session)
            .map(|(_id, event)| event.payload.clone())
            .collect()
    }

    /// Returns every event, with metadata, emitted for one Session in order.
    // Only the protocol_alignment target, which shares this fixture, reads events.
    #[allow(dead_code)]
    pub fn events(&self, session: &SessionId) -> Vec<AgentEvent> {
        self.0
            .lock()
            .expect("events")
            .iter()
            .filter(|(id, _event)| id == session)
            .map(|(_id, event)| event.clone())
            .collect()
    }

    /// Checks events emitted by real Kernel runs for a particular Session.
    // Only the protocol_alignment target, which shares this fixture, checks events.
    #[allow(dead_code)]
    pub fn saw(
        &self,
        session: &SessionId,
        check: impl Fn(&AgentEventPayload) -> bool,
    ) -> bool {
        self.0
            .lock()
            .expect("events")
            .iter()
            .any(|(id, e)| id == session && check(&e.payload))
    }
}

impl SessionEventSinks for RecordingSinks {
    /// Routes every Session into a correlated capture without changing execution.
    fn sink(&self, session_id: &SessionId) -> Arc<dyn EventSink> {
        Arc::new(RecordingSink {
            session: session_id.clone(),
            events: Arc::clone(&self.0),
        })
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    /// Retains a fully correlated event for lifecycle assertions.
    async fn emit(&self, event: AgentEvent) -> Result<(), SinkError> {
        self.events
            .lock()
            .expect("events")
            .push((self.session.clone(), event));
        Ok(())
    }
}

#[derive(Default)]
struct SequenceClock(Mutex<u64>);
impl Clock for SequenceClock {
    /// Advances time monotonically for durable ordering.
    fn now(&self) -> TimestampMs {
        let mut n = self.0.lock().expect("clock");
        *n += 1;
        TimestampMs::from(*n)
    }
}
impl IdGenerator for SequenceClock {
    /// Allocates deterministic unique identifiers for all runtime domains.
    fn next(&self, kind: IdKind) -> String {
        let mut n = self.0.lock().expect("ids");
        *n += 1;
        format!("{}-{n}", kind.prefix())
    }
}

/// Kernel queue and Run policies a test may override.
#[derive(Clone, Copy, Default)]
pub struct KernelPolicy {
    pub max_turns: config::TurnLimit,
    pub when_busy: WhenBusy,
    /// Overrides `subagent.max_agents`; `None` keeps the default of three.
    pub max_agents: Option<std::num::NonZeroUsize>,
    /// Turns off `<command_sessions>` notes for tests of that switch.
    pub disable_command_sessions: bool,
}

#[derive(typed_builder::TypedBuilder)]
pub struct TestKernel {
    pub kernel: Kernel,
    pub root: tempfile::TempDir,
    pub sinks: Arc<RecordingSinks>,
    pub model: Arc<RoutedModel>,
}

impl TestKernel {
    /// Constructs production Kernel components with only the model boundary scripted.
    pub fn new(
        route: impl Fn(&ModelRequest) -> Route + Send + Sync + 'static,
    ) -> Self {
        Self::with_extensions(
            route,
            Arc::new(StaticExtensionFactory::new(
                Default::default(),
                vec![Arc::new(|| Ok(Arc::new(StateModule)))],
            )),
        )
    }

    /// Reuses the Kernel fixture with an explicitly supplied extension registration.
    pub fn with_extensions(
        route: impl Fn(&ModelRequest) -> Route + Send + Sync + 'static,
        extensions: Arc<dyn extension::ExtensionFactory>,
    ) -> Self {
        Self::with_factories(
            route,
            extensions,
            |root, clock| {
                Arc::new(JsonlStoreFactory::new(root.join("sessions"), clock))
            },
            KernelPolicy::default(),
            protocol::CompactionPolicy::default(),
        )
    }

    /// Uses the default extensions with an explicit compaction policy.
    // Only the protocol_alignment target, which shares this fixture, tunes compaction.
    #[allow(dead_code)]
    pub fn with_compaction(
        route: impl Fn(&ModelRequest) -> Route + Send + Sync + 'static,
        compaction: protocol::CompactionPolicy,
    ) -> Self {
        Self::with_factories(
            route,
            Arc::new(StaticExtensionFactory::new(
                Default::default(),
                vec![Arc::new(|| Ok(Arc::new(StateModule)))],
            )),
            |root, clock| {
                Arc::new(JsonlStoreFactory::new(root.join("sessions"), clock))
            },
            KernelPolicy::default(),
            compaction,
        )
    }

    /// Substitutes only storage while retaining the production Kernel and tool pipeline.
    // Only the protocol_alignment target, which shares this fixture, swaps storage.
    #[allow(dead_code)]
    pub fn with_store(
        route: impl Fn(&ModelRequest) -> Route + Send + Sync + 'static,
        store: impl FnOnce(
            &std::path::Path,
            Arc<dyn Clock>,
        ) -> Arc<dyn store::StoreFactory>,
    ) -> Self {
        Self::with_factories(
            route,
            Arc::new(StaticExtensionFactory::default()),
            store,
            KernelPolicy::default(),
            protocol::CompactionPolicy::default(),
        )
    }

    /// Overrides Run limits and queue policies without changing production dependencies.
    pub fn with_policy(
        route: impl Fn(&ModelRequest) -> Route + Send + Sync + 'static,
        policy: KernelPolicy,
    ) -> Self {
        Self::with_factories(
            route,
            Arc::new(StaticExtensionFactory::default()),
            |root, clock| {
                Arc::new(JsonlStoreFactory::new(root.join("sessions"), clock))
            },
            policy,
            protocol::CompactionPolicy::default(),
        )
    }

    /// Composes the existing fixture with explicit extension and storage boundaries.
    fn with_factories(
        route: impl Fn(&ModelRequest) -> Route + Send + Sync + 'static,
        extensions: Arc<dyn extension::ExtensionFactory>,
        store: impl FnOnce(
            &std::path::Path,
            Arc<dyn Clock>,
        ) -> Arc<dyn store::StoreFactory>,
        policy: KernelPolicy,
        compaction: protocol::CompactionPolicy,
    ) -> Self {
        let root = tempfile::tempdir().expect("root");
        let model = Arc::new(RoutedModel {
            profile: PROFILE.clone(),
            route: Box::new(route),
            requests: Mutex::new(Vec::new()),
            preflight_failure: Mutex::new(None),
            sign_in_required: Arc::new(Mutex::new(None)),
            sign_in_wait: Mutex::new(None),
        });
        let clock: Arc<dyn Clock> = Arc::new(SequenceClock::default());
        let kernel = KernelFactory::builder()
            .max_turns(policy.max_turns)
            .when_busy(policy.when_busy)
            .subagent_max_agents(
                policy
                    .max_agents
                    .unwrap_or(config::SubagentConfig::default().max_agents),
            )
            .compaction_policy(compaction)
            .model_factory(Arc::new(StaticModelFactory(Arc::clone(&model))))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(store(root.path(), Arc::clone(&clock)))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.path().join("config"),
                Default::default(),
            )))
            .extension_factory(extensions)
            .clock(clock)
            .id_generator(Arc::new(SequenceClock::default()))
            .exec(Some(Arc::new(tools::exec::ExecSettings {
                config: config::ExecConfig::builder()
                    .shell_snapshot(false)
                    .command_sessions(!policy.disable_command_sessions)
                    .build(),
                snapshot_dir: root.path().join("shell_snapshots"),
            })))
            .build()
            .build()
            .expect("kernel");
        let sinks = Arc::new(RecordingSinks::default());
        kernel
            .install_event_sinks(
                Arc::clone(&sinks) as Arc<dyn SessionEventSinks>
            )
            .expect("sinks");
        Self::builder()
            .kernel(kernel)
            .root(root)
            .sinks(sinks)
            .model(model)
            .build()
    }

    /// Creates a normal owner Session in the test's isolated directory.
    pub async fn create(&self, id: &str) -> SessionId {
        let session = SessionId::try_from(id).expect("id");
        self.kernel
            .create_session(SessionCreateOptions {
                session_id: session.clone(),
                cwd: self.root.path().into(),
                parent_session_id: None,
            })
            .await
            .expect("create");
        session
    }

    /// Bounds asynchronous observations to expose hangs without unbounded test waits.
    pub async fn wait_until(&self, check: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !check() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("condition timed out");
    }

    /// Starts one structured Run in the background so a test can act while it waits.
    pub fn start(
        &self,
        session: &SessionId,
        text: &str,
    ) -> tokio::task::JoinHandle<Result<protocol::RunResult, kernel::KernelError>>
    {
        let kernel = self.kernel.clone();
        let sink = self.sinks.sink(session);
        let request = protocol::RunRequest {
            session_id: session.clone(),
            input: protocol::RunInput::Blocks(vec![ContentBlock::Text {
                text: text.into(),
            }]),
        };
        tokio::spawn(async move { kernel.run(request, sink).await })
    }

    /// Runs structured input to settlement; text never dispatches user bash
    /// or Slash Commands, as for any internal caller.
    // The exec and protocol_alignment targets, which share this fixture, use it.
    #[allow(dead_code)]
    pub async fn run_text(
        &self,
        session: &SessionId,
        text: &str,
    ) -> Result<protocol::RunResult, kernel::KernelError> {
        self.kernel
            .run(
                protocol::RunRequest {
                    session_id: session.clone(),
                    input: protocol::RunInput::Blocks(vec![
                        ContentBlock::Text { text: text.into() },
                    ]),
                },
                self.sinks.sink(session),
            )
            .await
    }

    /// Delivers one mail from `author` to a Session through the public primitive.
    pub async fn mail(
        &self,
        recipient: &SessionId,
        author: &str,
        kind: protocol::MailKind,
        payload: &str,
    ) -> kernel::MailPlacement {
        self.kernel
            .deliver_mail(
                kernel::MailDelivery::builder()
                    .recipient(recipient.clone())
                    .mail(
                        protocol::AgentMail::builder()
                            .author(
                                protocol::AgentPath::try_from(author)
                                    .expect("author"),
                            )
                            .recipient(protocol::AgentPath::root())
                            .kind(kind)
                            .payload(payload.into())
                            .build(),
                    )
                    .build(),
            )
            .await
            .expect("deliver")
    }

    /// Reads the model text of every persisted agent mail in one Session.
    pub fn agent_mails(&self, session: &SessionId) -> Vec<String> {
        self.kernel
            .session_transcript(session)
            .expect("transcript")
            .iter()
            .filter(|m| {
                matches!(&m.content, MessageContent::Extension { extension }
                    if extension.custom_type == protocol::AGENT_MESSAGE_TYPE)
            })
            .map(|m| text(&m.content))
            .collect()
    }

    /// Lists persisted ownership edges through the public metadata API.
    pub fn owned_children(&self, owner: &SessionId) -> Vec<SessionSummary> {
        self.kernel
            .list_sessions(Some(self.root.path()))
            .expect("list")
            .into_iter()
            .filter(|s| {
                s.ownership.as_ref().is_some_and(|o| &o.session_id == owner)
            })
            .collect()
    }

    /// Reads the user messages of a Session's real transcript.
    // Only the protocol_alignment target, which shares this fixture, reads them.
    #[allow(dead_code)]
    pub fn user_texts(&self, session: &SessionId) -> Vec<String> {
        self.kernel
            .session_transcript(session)
            .expect("transcript")
            .iter()
            .filter(|m| matches!(m.content, MessageContent::User { .. }))
            .map(|m| text(&m.content))
            .collect()
    }
}

/// Scripts one provider turn that calls the given tools.
pub fn calls(calls: &[(&str, &str, serde_json::Value)]) -> Route {
    let mut events = calls
        .iter()
        .map(|(id, name, arguments)| {
            ModelStreamEvent::ToolCall(
                protocol::ToolCall::builder()
                    .tool_call_id(
                        protocol::ToolCallId::try_from(*id).expect("id"),
                    )
                    .name((*name).into())
                    .arguments(arguments.clone())
                    .build(),
            )
        })
        .collect::<Vec<_>>();
    events.push(finished(StopReason::ToolUse));
    Route::Events(events)
}

/// Whether the result of `call_id` in `session` is marked as an error.
pub fn tool_failed(
    test: &TestKernel,
    session: &SessionId,
    call_id: &str,
) -> bool {
    test.kernel
        .session_transcript(session)
        .expect("transcript")
        .iter()
        .rev()
        .find_map(|message| match &message.content {
            MessageContent::ToolResult {
                tool_call_id,
                is_error,
                ..
            } if tool_call_id.as_str() == call_id => Some(*is_error),
            _ => None,
        })
        .expect("tool result")
}

/// Reads the latest tool result with `call_id` from a Session's transcript,
/// so scripted calls may reuse an id across prompts.
pub fn tool_result(
    test: &TestKernel,
    session: &SessionId,
    call_id: &str,
) -> (String, Option<protocol::ToolResultDetails>) {
    test.kernel
        .session_transcript(session)
        .expect("transcript")
        .iter()
        .rev()
        .find_map(|message| match &message.content {
            MessageContent::ToolResult {
                tool_call_id,
                blocks,
                details,
                ..
            } if tool_call_id.as_str() == call_id => Some((
                blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
                details.clone(),
            )),
            _ => None,
        })
        .expect("tool result")
}

/// Exposes configuration mutations through the same Extension Host used in production.
struct StateModule;
impl extension::ExtensionModule for StateModule {
    /// Identifies the test's settings extension.
    fn descriptor(&self) -> protocol::ExtensionDescriptor {
        protocol::ExtensionDescriptor {
            id: protocol::ExtensionId::try_from("agent-state").expect("id"),
            name: "Agent state".into(),
            version: "1".into(),
        }
    }
    /// Registers a command that copies real configured choices into the owner.
    fn register(
        &self,
        registrar: &mut extension::ExtensionRegistrar,
    ) -> Result<(), extension::ExtensionError> {
        registrar.register_command(
            protocol::ExtensionCommandDefinition {
                name: "agent-state".into(),
                description: None,
                argument_hint: None,
            },
            Self,
        )
    }
}
#[async_trait]
impl extension::ExtensionCommandHandler for StateModule {
    /// Selects a distinct model, thinking level, and tool policy before spawning.
    async fn handle(
        &self,
        _arguments: &str,
        _parameters: &serde_json::Value,
        context: &extension::ExtensionCommandContext,
    ) -> Result<(), extension::ExtensionError> {
        context
            .event
            .set_model("fixture", "secondary")
            .await
            .map_err(|e| extension::ExtensionError::Handler(e.to_string()))?;
        context
            .event
            .set_thinking_level(protocol::ThinkingLevel::High)
            .await
            .map_err(|e| extension::ExtensionError::Handler(e.to_string()))?;
        context
            .event
            .set_active_tools(vec!["read".into()])
            .await
            .map_err(|e| extension::ExtensionError::Handler(e.to_string()))
    }
}
