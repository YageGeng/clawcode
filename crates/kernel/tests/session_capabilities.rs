use std::collections::VecDeque;
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use extension::{
    ExtensionCommandContext, ExtensionCommandHandler, ExtensionContext,
    ExtensionError, ExtensionHandler, ExtensionModule, ExtensionRegistrar,
    ModelSelectPoint, ProjectTrustPoint, ResourcesDiscoverPoint,
    SessionBeforeSwitchPoint, SessionInfoChangedPoint, SessionStartPoint,
    StaticExtensionFactory, ThinkingLevelSelectPoint,
};
use futures::{Stream, stream};
use kernel::{
    EventSink, Kernel, KernelFactory, Model, ModelError, ModelFactory,
};
use mcp::{
    McpError, McpFactory, McpMrtrPolicy, McpSession, McpSessionRequest,
    McpStdioTransport, McpTransport, RmcpConnector, RuntimeMcpServer,
    SessionMcpFactory,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, ContentBlock, ExtensionDescriptor,
    ExtensionId, IdGenerator, IdKind, McpProtocolVersion, McpServerId,
    McpServerState, MessageContent, ModelFailure, ModelFinal,
    ModelInputModalities, ModelInputModality, ModelProfile, ModelRequest,
    ModelRetryDisposition, ModelStreamEvent, ModelUsage, QueueKind, RunInput,
    RunRequest, SessionId, SessionTitle, SkillDiagnosticCode,
    SlashCommandSource, StopReason, TimestampMs,
};
use skill::FilesystemSkillFactory;
use store::{Clock, JsonlStoreFactory, SessionCreateOptions};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

type TestModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>>;

/// Shared deterministic capabilities used by session-level model fixtures.
static TEST_MODEL_PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("session".to_string())
        .display_name("Session fixture".to_string())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .build()
});

/// Shared deterministic capabilities for fixtures that must retain images.
static IMAGE_TEST_MODEL_PROFILE: LazyLock<ModelProfile> = LazyLock::new(|| {
    ModelProfile::builder()
        .provider_id("fixture".to_string())
        .model_id("session-image".to_string())
        .display_name("Session image fixture".to_string())
        .context_tokens(128_000)
        .max_output_tokens(8_000)
        .input(
            ModelInputModalities::try_from(vec![
                ModelInputModality::Text,
                ModelInputModality::Image,
            ])
            .expect("image input modalities"),
        )
        .build()
});

/// Supplies strictly increasing deterministic timestamps to all test components.
struct StepClock(AtomicU64);

impl Clock for StepClock {
    /// Advances the clock by one millisecond for each observation.
    fn now(&self) -> TimestampMs {
        TimestampMs::from(self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// Supplies readable identifiers while preserving uniqueness across resume.
struct SequentialIds(AtomicU64);

impl IdGenerator for SequentialIds {
    /// Generates one prefixed identifier for the requested domain type.
    fn next(&self, kind: IdKind) -> String {
        let value = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        format!("{}-{value}", kind.prefix())
    }
}

/// Returns one shared deterministic model from the kernel factory boundary.
struct StaticModelFactory(Arc<dyn Model>);

impl ModelFactory for StaticModelFactory {
    /// Clones the configured model without contacting a provider.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0))
    }
}

/// Holds provider output open until cancellation while signaling stream entry.
struct PendingModel {
    entered: Arc<Notify>,
}

#[async_trait]
impl Model for PendingModel {
    /// Returns the stable profile used while testing queue cancellation.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that the pending local model is ready without provider access.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Signals that the run is active and returns a never-ending stream.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        self.entered.notify_one();
        Ok(Box::pin(stream::pending()))
    }
}

/// Holds the first request open and captures each request that follows it.
#[derive(Default)]
struct GatedCaptureModel {
    requests: Mutex<Vec<ModelRequest>>,
    first_entered: Notify,
    first_release: Notify,
}

/// Holds the first two model Turns independently for queue-correlation races.
#[derive(Default)]
struct TwoTurnGatedModel {
    calls: AtomicU64,
    entered: [Notify; 2],
    releases: [Notify; 2],
}

#[async_trait]
impl Model for TwoTurnGatedModel {
    /// Returns the stable profile used by queue diagnostic assertions.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that the local gated model has no provider dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Pauses the first two requests until the test advances each active Turn.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst) as usize;
        if let (Some(entered), Some(release)) =
            (self.entered.get(index), self.releases.get(index))
        {
            entered.notify_one();
            release.notified().await;
        }
        Ok(Box::pin(stream::iter([Ok(ScriptedModel::finished(
            StopReason::EndTurn,
        ))])))
    }
}

#[async_trait]
impl Model for GatedCaptureModel {
    /// Returns the stable profile used by queued follow-up assertions.
    fn profile(&self) -> &ModelProfile {
        &IMAGE_TEST_MODEL_PROFILE
    }

    /// Confirms that the gated local model has no provider dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Captures every request while pausing only the first model turn.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        let is_first = {
            let mut requests = self.requests.lock().expect("request lock");
            requests.push(request);
            requests.len() == 1
        };
        if is_first {
            self.first_entered.notify_one();
            self.first_release.notified().await;
        }
        Ok(Box::pin(stream::iter([Ok(ScriptedModel::finished(
            StopReason::EndTurn,
        ))])))
    }
}

/// Returns scripted local responses in request order.
struct ScriptedModel {
    scripts: Mutex<VecDeque<Vec<ModelStreamEvent>>>,
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

/// Returns one immutable MCP status snapshot without external connections.
struct StaticMcpFactory;

#[async_trait]
impl mcp::McpFactory for StaticMcpFactory {
    /// Produces one disabled Server snapshot without opening an external transport.
    async fn create(
        &self,
        request: mcp::McpSessionRequest,
    ) -> Result<mcp::McpSession, mcp::McpError> {
        SessionMcpFactory::new(
            vec![
                RuntimeMcpServer::builder()
                    .server_id(
                        McpServerId::try_from("docs").expect("Server id"),
                    )
                    .enabled(false)
                    .protocol(McpProtocolVersion::V2025_11_25)
                    .startup_timeout(Duration::from_secs(1))
                    .request_timeout(Duration::from_secs(1))
                    .mrtr(McpMrtrPolicy {
                        max_rounds: NonZeroU32::new(1)
                            .expect("non-zero rounds"),
                        total_timeout: Duration::from_secs(1),
                    })
                    .transport(McpTransport::Stdio(Box::new(
                        McpStdioTransport {
                            command: "unused".to_string(),
                            args: Vec::new(),
                            env: Default::default(),
                        },
                    )))
                    .build(),
            ],
            Arc::new(RmcpConnector::default()),
        )
        .create(request)
        .await
    }
}

#[async_trait]
impl Model for ScriptedModel {
    /// Returns the stable profile used by scripted session requests.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that scripted responses have no external readiness dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Streams the next complete response without a real provider.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        let script = self
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
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }
}

/// Records complete provider requests while returning successful empty responses.
struct PromptCaptureModel(Mutex<Vec<ModelRequest>>);

#[async_trait]
impl Model for PromptCaptureModel {
    /// Returns the shared profile used by Session Prompt assertions.
    fn profile(&self) -> &ModelProfile {
        &TEST_MODEL_PROFILE
    }

    /// Confirms that Prompt capture requires no external provider.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Captures the complete request before ending the current Turn.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<TestModelStream, ModelError> {
        self.0.lock().expect("request lock").push(request);
        Ok(Box::pin(stream::iter([Ok(ScriptedModel::finished(
            StopReason::EndTurn,
        ))])))
    }
}

/// Records domain events for title-notification assertions.
#[derive(Default)]
struct RecordingSink(Mutex<Vec<AgentEvent>>);

#[async_trait]
impl EventSink for RecordingSink {
    /// Retains each event in protocol delivery order.
    async fn emit(&self, event: AgentEvent) -> Result<(), kernel::SinkError> {
        self.0.lock().expect("sink lock").push(event);
        Ok(())
    }
}

#[derive(Clone)]
struct RecordingExtension(Arc<Mutex<Vec<&'static str>>>);

impl ExtensionModule for RecordingExtension {
    /// Declares the deterministic test extension identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("recording").expect("extension id"),
            name: "Recording".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers only the typed session points asserted by this test.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<ModelSelectPoint, _>(self.clone())?;
        registrar.on::<ThinkingLevelSelectPoint, _>(self.clone())?;
        registrar.on::<SessionInfoChangedPoint, _>(self.clone())?;
        registrar.on::<ProjectTrustPoint, _>(self.clone())?;
        registrar.on::<SessionStartPoint, _>(self.clone())?;
        registrar.on::<ResourcesDiscoverPoint, _>(self.clone())?;
        registrar.on::<SessionBeforeSwitchPoint, _>(self.clone())
    }
}

macro_rules! record_session_observer {
    ($point:ty, $event:ty, $name:literal) => {
        #[async_trait]
        impl ExtensionHandler<$point> for RecordingExtension {
            /// Records one typed session observer invocation.
            async fn handle(
                &self,
                _event: &$event,
                _context: &ExtensionContext,
            ) -> Result<(), ExtensionError> {
                self.0.lock().expect("extension lock").push($name);
                Ok(())
            }
        }
    };
}

record_session_observer!(
    ModelSelectPoint,
    protocol::ModelSelectEvent,
    "model_select"
);
record_session_observer!(
    ThinkingLevelSelectPoint,
    protocol::ThinkingLevelSelectEvent,
    "thinking_level_select"
);
record_session_observer!(
    SessionInfoChangedPoint,
    protocol::SessionInfoChangedEvent,
    "session_info_changed"
);
record_session_observer!(
    SessionStartPoint,
    protocol::SessionStartEvent,
    "session_start"
);

#[async_trait]
impl ExtensionHandler<ProjectTrustPoint> for RecordingExtension {
    /// Records trust evaluation while allowing local resource discovery.
    async fn handle(
        &self,
        _event: &protocol::ProjectTrustEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::ProjectTrustResult, ExtensionError> {
        self.0.lock().expect("extension lock").push("project_trust");
        Ok(protocol::ProjectTrustResult {
            trusted: protocol::ProjectTrustDecision::Yes,
            remember: false,
        })
    }
}

#[async_trait]
impl ExtensionHandler<ResourcesDiscoverPoint> for RecordingExtension {
    /// Records resource discovery without contributing additional paths.
    async fn handle(
        &self,
        _event: &protocol::ResourcesDiscoverEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::ResourcesDiscoverResult, ExtensionError> {
        self.0
            .lock()
            .expect("extension lock")
            .push("resources_discover");
        Ok(protocol::ResourcesDiscoverResult::default())
    }
}

#[async_trait]
impl ExtensionHandler<SessionBeforeSwitchPoint> for RecordingExtension {
    /// Records the cancellable switch point while allowing the operation.
    async fn handle(
        &self,
        _event: &protocol::SessionBeforeSwitchEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::SessionCancelResult, ExtensionError> {
        self.0
            .lock()
            .expect("extension lock")
            .push("session_before_switch");
        Ok(protocol::SessionCancelResult::default())
    }
}

/// Cancels every Resume switch before the runtime can be registered.
#[derive(Clone)]
struct CancelResumeExtension;

impl ExtensionModule for CancelResumeExtension {
    /// Declares the Resume cancellation fixture.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("cancel-resume").expect("extension id"),
            name: "Cancel Resume".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers only the pre-switch cancellation point.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<SessionBeforeSwitchPoint, _>(self.clone())
    }
}

#[async_trait]
impl ExtensionHandler<SessionBeforeSwitchPoint> for CancelResumeExtension {
    /// Cancels the attempted switch so cleanup runs before registration.
    async fn handle(
        &self,
        _event: &protocol::SessionBeforeSwitchEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::SessionCancelResult, ExtensionError> {
        Ok(protocol::SessionCancelResult { cancel: true })
    }
}

/// Pauses Resume before registration so concurrent construction can be tested.
#[derive(Clone)]
struct GateResumeExtension {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl ExtensionModule for GateResumeExtension {
    /// Declares the concurrent Resume fixture identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("gate-resume").expect("extension id"),
            name: "Gate Resume".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers only the pre-switch gate used by Resume.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<SessionBeforeSwitchPoint, _>(self.clone())
    }
}

#[async_trait]
impl ExtensionHandler<SessionBeforeSwitchPoint> for GateResumeExtension {
    /// Blocks only persisted-session Resume attempts before registration.
    async fn handle(
        &self,
        event: &protocol::SessionBeforeSwitchEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::SessionCancelResult, ExtensionError> {
        if event.reason == protocol::SessionSwitchReason::Resume {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(protocol::SessionCancelResult::default())
    }
}

/// Pauses new Session startup after registration but before resources are ready.
#[derive(Clone)]
struct GateSessionStartExtension {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl ExtensionModule for GateSessionStartExtension {
    /// Declares the startup publication fixture identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("gate-session-start")
                .expect("extension id"),
            name: "Gate Session Start".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers only the Session startup gate used by creation tests.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<SessionStartPoint, _>(self.clone())
    }
}

#[async_trait]
impl ExtensionHandler<SessionStartPoint> for GateSessionStartExtension {
    /// Blocks new Session startup at the first hook after map registration.
    async fn handle(
        &self,
        event: &protocol::SessionStartEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        if event.reason == protocol::SessionStartReason::New {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// Records every Session shutdown token created by the wrapped MCP factory.
struct ObservedMcpFactory {
    inner: SessionMcpFactory,
    shutdowns: Arc<Mutex<Vec<CancellationToken>>>,
}

#[async_trait]
impl McpFactory for ObservedMcpFactory {
    /// Captures the shutdown token before creating an empty MCP Session.
    async fn create(
        &self,
        request: McpSessionRequest,
    ) -> Result<McpSession, McpError> {
        self.shutdowns
            .lock()
            .expect("shutdown token lock")
            .push(request.shutdown.clone());
        self.inner.create(request).await
    }
}

/// Rejects project trust and records whether resource discovery was attempted.
#[derive(Clone)]
struct DenyProjectResources {
    events: Arc<Mutex<Vec<&'static str>>>,
    skill_path: PathBuf,
}

impl ExtensionModule for DenyProjectResources {
    /// Declares the trust-boundary fixture identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("deny-project-resources")
                .expect("extension id"),
            name: "Deny project resources".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers trust evaluation and independently owned Extension resources.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<ProjectTrustPoint, _>(self.clone())?;
        registrar.on::<ResourcesDiscoverPoint, _>(self.clone())
    }
}

#[async_trait]
impl ExtensionHandler<ProjectTrustPoint> for DenyProjectResources {
    /// Rejects project-controlled Prompt, Skill, and Extension resource roots.
    async fn handle(
        &self,
        _event: &protocol::ProjectTrustEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::ProjectTrustResult, ExtensionError> {
        self.events
            .lock()
            .expect("trust event lock")
            .push("project_trust");
        Ok(protocol::ProjectTrustResult {
            trusted: protocol::ProjectTrustDecision::No,
            remember: false,
        })
    }
}

#[async_trait]
impl ExtensionHandler<ResourcesDiscoverPoint> for DenyProjectResources {
    /// Contributes a non-project Skill even when project resources are denied.
    async fn handle(
        &self,
        _event: &protocol::ResourcesDiscoverEvent,
        _context: &ExtensionContext,
    ) -> Result<protocol::ResourcesDiscoverResult, ExtensionError> {
        self.events
            .lock()
            .expect("trust event lock")
            .push("resources_discover");
        Ok(protocol::ResourcesDiscoverResult {
            skill_paths: vec![self.skill_path.clone()],
            prompt_paths: Vec::new(),
        })
    }
}

/// No-op command used to prove Extension commands cannot enter a run queue.
struct QueueRejectedCommand;

#[async_trait]
impl ExtensionCommandHandler for QueueRejectedCommand {
    /// Completes immediately if invoked outside the queue path.
    async fn handle(
        &self,
        _arguments: &str,
        _parameters: &serde_json::Value,
        _context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        Ok(())
    }
}

/// Registers the Extension command reserved by the queue rejection test.
struct QueueCommandModule;

impl ExtensionModule for QueueCommandModule {
    /// Declares the queue command fixture identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("queue-command").expect("extension id"),
            name: "Queue command".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers one command that must execute immediately or be rejected.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.register_command(
            protocol::ExtensionCommandDefinition {
                name: "not-queueable".to_string(),
                description: None,
                argument_hint: None,
            },
            QueueRejectedCommand,
        )
    }
}

/// Registers a fixed group of commands under one Extension identity.
struct AvailableCommandModule {
    id: &'static str,
    commands: Vec<protocol::ExtensionCommandDefinition>,
}

impl ExtensionModule for AvailableCommandModule {
    /// Declares the command projection fixture identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from(self.id).expect("extension id"),
            name: self.id.to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers every fixed command with a no-op handler.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        for command in &self.commands {
            registrar
                .register_command(command.clone(), QueueRejectedCommand)?;
        }
        Ok(())
    }
}

/// Builds one kernel with deterministic storage, tools, clocks, and identifiers.
fn build_kernel(
    root: PathBuf,
    model: Arc<dyn Model>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    skill_root: Option<PathBuf>,
) -> Arc<Kernel> {
    let builder = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.clone(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(ids)
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.join("config"),
            protocol::PromptPolicy::default(),
        )));
    let factory = match skill_root {
        Some(skill_root) => builder
            .skill_factory(Some(Arc::new(filesystem_skill_factory(skill_root))))
            .build(),
        None => builder.build(),
    };
    Arc::new(factory.build().expect("build kernel"))
}

/// Builds a kernel whose Resume hook can be paused at a deterministic boundary.
fn build_gated_resume_kernel(
    root: &Path,
    clock: Arc<dyn Clock>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    shutdowns: Arc<Mutex<Vec<CancellationToken>>>,
) -> Arc<Kernel> {
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(StaticModelFactory(model)))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::new(
                protocol::StaticExtensionRegistration::default(),
                vec![Arc::new({
                    let entered = Arc::clone(&entered);
                    let release = Arc::clone(&release);
                    move || {
                        Ok(Arc::new(GateResumeExtension {
                            entered: Arc::clone(&entered),
                            release: Arc::clone(&release),
                        })
                            as Arc<dyn ExtensionModule>)
                    }
                })],
            )))
            .mcp_factory(Some(Arc::new(ObservedMcpFactory {
                inner: SessionMcpFactory::new(
                    Vec::new(),
                    Arc::new(RmcpConnector::default()),
                ),
                shutdowns,
            })))
            .clock(clock)
            .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .build()
            .build()
            .expect("build kernel"),
    )
}

/// Builds a kernel whose new-Session startup hook pauses after registration.
fn build_gated_start_kernel(
    root: &Path,
    clock: Arc<dyn Clock>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
) -> Arc<Kernel> {
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(StaticModelFactory(model)))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::new(
                protocol::StaticExtensionRegistration::default(),
                vec![Arc::new({
                    let entered = Arc::clone(&entered);
                    let release = Arc::clone(&release);
                    move || {
                        Ok(Arc::new(GateSessionStartExtension {
                            entered: Arc::clone(&entered),
                            release: Arc::clone(&release),
                        })
                            as Arc<dyn ExtensionModule>)
                    }
                })],
            )))
            .clock(clock)
            .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .build()
            .build()
            .expect("build kernel"),
    )
}

/// Builds a filesystem Skill Factory with an isolated unused home root.
fn filesystem_skill_factory(global_root: PathBuf) -> FilesystemSkillFactory {
    let user_home = global_root.join("test-home");
    FilesystemSkillFactory::builder()
        .global_root(global_root)
        .user_home(user_home)
        .build()
}

/// Resume cancellation shuts down its unregistered MCP runtime without deleting Store.
#[tokio::test]
async fn resume_cancellation_shuts_down_unregistered_mcp() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(4_000)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    let shutdowns = Arc::new(Mutex::new(Vec::new()));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            protocol::StaticExtensionRegistration::default(),
            vec![Arc::new(|| {
                Ok(Arc::new(CancelResumeExtension) as Arc<dyn ExtensionModule>)
            })],
        )))
        .mcp_factory(Some(Arc::new(ObservedMcpFactory {
            inner: SessionMcpFactory::new(
                Vec::new(),
                Arc::new(RmcpConnector::default()),
            ),
            shutdowns: Arc::clone(&shutdowns),
        })))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-cancel-resume").expect("session id");
    let session_path = kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    kernel
        .close_session(&session_id)
        .await
        .expect("close initial session");

    let error = kernel
        .resume_session(session_id, cwd)
        .await
        .expect_err("Resume must be cancelled");
    assert!(matches!(error, kernel::KernelError::ExtensionBlocked(_)));
    let shutdowns = shutdowns.lock().expect("shutdown token lock");
    assert_eq!(shutdowns.len(), 2);
    assert!(shutdowns[1].is_cancelled());
    assert!(session_path.exists());
}

/// Rejects a second Resume before it can construct another runtime for the same id.
#[tokio::test]
async fn concurrent_resume_reserves_session_before_construction() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(4_500)));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let shutdowns = Arc::new(Mutex::new(Vec::new()));
    let kernel = build_gated_resume_kernel(
        root.path(),
        clock,
        Arc::clone(&entered),
        Arc::clone(&release),
        Arc::clone(&shutdowns),
    );
    let session_id =
        SessionId::try_from("session-concurrent-resume").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    kernel
        .close_session(&session_id)
        .await
        .expect("close initial session");

    let first_kernel = Arc::clone(&kernel);
    let first_session_id = session_id.clone();
    let first_cwd = cwd.clone();
    let first = tokio::spawn(async move {
        first_kernel
            .resume_session(first_session_id, first_cwd)
            .await
    });
    entered.notified().await;

    let second_kernel = Arc::clone(&kernel);
    let second_session_id = session_id.clone();
    let second = tokio::spawn(async move {
        second_kernel.resume_session(second_session_id, cwd).await
    });
    let second_result =
        tokio::time::timeout(Duration::from_secs(1), second).await;
    if second_result.is_err() {
        release.notify_waiters();
        first
            .await
            .expect("join first Resume")
            .expect("first Resume");
        panic!(
            "second Resume reached construction instead of observing the reservation"
        );
    }
    let second_error = second_result
        .expect("second Resume timeout")
        .expect("join second Resume")
        .expect_err("second Resume must be rejected");
    assert!(matches!(
        second_error,
        kernel::KernelError::DuplicateSession(id) if id == session_id
    ));
    assert_eq!(shutdowns.lock().expect("shutdown token lock").len(), 2);

    release.notify_waiters();
    first
        .await
        .expect("join first Resume")
        .expect("first Resume");
}

/// Rejects Delete while an unregistered Resume owns the same Session identifier.
#[tokio::test]
async fn delete_rejects_session_reserved_by_resume_construction() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(4_600)));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let kernel = build_gated_resume_kernel(
        root.path(),
        clock,
        Arc::clone(&entered),
        Arc::clone(&release),
        Arc::new(Mutex::new(Vec::new())),
    );
    let session_id =
        SessionId::try_from("session-delete-reserved").expect("session id");
    let session_path = kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    kernel
        .close_session(&session_id)
        .await
        .expect("close initial session");

    let resume_kernel = Arc::clone(&kernel);
    let resume_session_id = session_id.clone();
    let resume = tokio::spawn(async move {
        resume_kernel.resume_session(resume_session_id, cwd).await
    });
    entered.notified().await;

    let delete_result = kernel.delete_session(&session_id).await;
    release.notify_waiters();
    resume
        .await
        .expect("join Resume")
        .expect("complete reserved Resume");

    assert!(matches!(
        delete_result,
        Err(kernel::KernelError::DuplicateSession(id)) if id == session_id
    ));
    assert!(session_path.exists());
}

/// Rejects public operations while a registered Session is still starting.
#[tokio::test]
async fn starting_session_rejects_close_until_resources_are_ready() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(4_650)));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let kernel = build_gated_start_kernel(
        root.path(),
        clock,
        Arc::clone(&entered),
        Arc::clone(&release),
    );
    let session_id =
        SessionId::try_from("session-starting-close").expect("session id");
    let create_kernel = Arc::clone(&kernel);
    let create_session_id = session_id.clone();
    let create = tokio::spawn(async move {
        create_kernel
            .create_session(SessionCreateOptions {
                session_id: create_session_id,
                cwd,
                parent_session_id: None,
            })
            .await
    });
    entered.notified().await;

    let close_result = kernel.close_session(&session_id).await;
    release.notify_waiters();
    let create_result = create.await.expect("join Session creation");

    assert_eq!(
        close_result
            .expect_err("starting Session must reject Close")
            .to_string(),
        "session is starting"
    );
    create_result.expect("complete Session creation after startup release");
}

/// Rejects a live Resume that loses the lifecycle race while its hook is paused.
#[tokio::test]
async fn live_resume_rechecks_lifecycle_after_before_switch_hook() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(4_700)));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let kernel = build_gated_resume_kernel(
        root.path(),
        clock,
        Arc::clone(&entered),
        Arc::clone(&release),
        Arc::new(Mutex::new(Vec::new())),
    );
    let session_id =
        SessionId::try_from("session-live-resume-close").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");

    let resume_kernel = Arc::clone(&kernel);
    let resume_session_id = session_id.clone();
    let resume_cwd = cwd.clone();
    let resume = tokio::spawn(async move {
        resume_kernel
            .resume_session(resume_session_id, resume_cwd)
            .await
    });
    entered.notified().await;
    kernel
        .close_session(&session_id)
        .await
        .expect("close live session while Resume hook is paused");
    release.notify_waiters();

    let resume_error = resume
        .await
        .expect("join live Resume")
        .expect_err("Resume must observe the completed close");
    assert!(matches!(resume_error, kernel::KernelError::SessionClosing));
}

/// Session creation, metadata changes, and resume dispatch their declared hooks.
#[tokio::test]
async fn session_management_dispatches_declared_extension_hooks() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(5_000)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    let extension_events = Arc::new(Mutex::new(Vec::new()));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
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
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-extension-hooks").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    kernel
        .rename_session(
            &session_id,
            SessionTitle::try_from("Renamed").expect("title"),
        )
        .await
        .expect("rename session");
    kernel
        .close_session(&session_id)
        .await
        .expect("close session");
    kernel
        .resume_session(session_id, cwd)
        .await
        .expect("resume session");

    let events = extension_events.lock().expect("extension lock");
    assert_eq!(
        &events[..5],
        [
            "project_trust",
            "session_start",
            "resources_discover",
            "model_select",
            "thinking_level_select",
        ]
    );
    for expected in [
        "project_trust",
        "session_start",
        "resources_discover",
        "model_select",
        "thinking_level_select",
        "session_info_changed",
        "session_before_switch",
    ] {
        assert!(events.contains(&expected), "missing {expected}");
    }
}

/// Project trust denial excludes project roots without suppressing Extension resources.
#[tokio::test]
async fn denied_project_trust_excludes_project_prompt_and_skill_resources() {
    let root = tempfile::tempdir().expect("store root");
    let config_root = root.path().join("config");
    let cwd = root.path().join("workspace");
    let global_skill = config_root.join("skills/global/SKILL.md");
    let project_skill = cwd.join(".pi/skills/project/SKILL.md");
    let extension_skill =
        root.path().join("extension/skills/extension/SKILL.md");
    fs::create_dir_all(global_skill.parent().expect("global Skill parent"))
        .expect("create global Skill parent");
    fs::create_dir_all(project_skill.parent().expect("project Skill parent"))
        .expect("create project Skill parent");
    fs::create_dir_all(
        extension_skill.parent().expect("Extension Skill parent"),
    )
    .expect("create Extension Skill parent");
    fs::write(
        &global_skill,
        "---\nname: global\ndescription: Global Skill\n---\nglobal\n",
    )
    .expect("write global Skill");
    fs::write(
        &project_skill,
        "---\nname: project\ndescription: Project Skill\n---\nproject\n",
    )
    .expect("write project Skill");
    fs::write(
        &extension_skill,
        "---\nname: extension\ndescription: Extension Skill\n---\nextension\n",
    )
    .expect("write Extension Skill");
    fs::write(config_root.join("AGENTS.md"), "GLOBAL INSTRUCTION")
        .expect("write global instruction");
    fs::write(cwd.join("AGENTS.md"), "PROJECT INSTRUCTION")
        .expect("write project instruction");

    let model = Arc::new(PromptCaptureModel(Mutex::new(Vec::new())));
    let trust_events = Arc::new(Mutex::new(Vec::new()));
    let module_events = Arc::clone(&trust_events);
    let module_skill = extension_skill.clone();
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(6_000)));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(
            Arc::clone(&model) as Arc<dyn Model>
        )))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            protocol::StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(DenyProjectResources {
                    events: Arc::clone(&module_events),
                    skill_path: module_skill.clone(),
                }) as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            config_root.clone(),
            protocol::PromptPolicy::default(),
        )))
        .skill_factory(Some(Arc::new(filesystem_skill_factory(config_root))))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-untrusted-project").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create untrusted project Session");
    kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "inspect resources".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("run untrusted project Session");

    assert_eq!(
        *trust_events.lock().expect("trust event lock"),
        vec!["project_trust", "resources_discover"]
    );
    assert_eq!(
        kernel
            .skills(&session_id)
            .expect("effective Skills")
            .skills
            .into_iter()
            .map(|skill| skill.name)
            .collect::<Vec<_>>(),
        vec!["extension", "global"]
    );
    let requests = model.0.lock().expect("request lock");
    let system = requests[0]
        .messages
        .iter()
        .find_map(|message| match &message.content {
            MessageContent::System { blocks } => blocks[0].text(),
            _ => None,
        })
        .expect("System Prompt");
    assert!(system.contains("GLOBAL INSTRUCTION"));
    assert!(!system.contains("PROJECT INSTRUCTION"));
}

/// Available Commands preserve execution precedence, aliases, and metadata.
#[tokio::test]
async fn available_commands_merge_session_sources_deterministically() {
    let root = tempfile::tempdir().expect("store root");
    let config_root = root.path().join("config");
    let cwd = root.path().join("workspace");
    let skill = config_root.join("skills/review/SKILL.md");
    let prompts = config_root.join("prompts");
    fs::create_dir_all(skill.parent().expect("Skill parent"))
        .expect("create Skill parent");
    fs::create_dir_all(&prompts).expect("create Prompt root");
    fs::create_dir_all(&cwd).expect("create project cwd");
    fs::write(
        &skill,
        "---\nname: review\ndescription: Review Skill\n---\nreview\n",
    )
    .expect("write Skill");
    fs::write(
        prompts.join("review.md"),
        "---\ndescription: Review Template\nargument-hint: <file>\n---\ntemplate\n",
    )
    .expect("write review Template");
    fs::write(
        prompts.join("skill:review.md"),
        "---\ndescription: Shadowed Template\n---\nshadowed\n",
    )
    .expect("write shadowed Template");
    fs::write(
        prompts.join("inspect.md"),
        "---\ndescription: Shadowed inspect\n---\nshadowed\n",
    )
    .expect("write inspect Template");
    fs::write(
        prompts.join("shared.md"),
        "---\ndescription: Unreachable shared Template\n---\nshadowed\n",
    )
    .expect("write shared Template");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(6_500)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            config_root.clone(),
            protocol::PromptPolicy::default(),
        )))
        .skill_factory(Some(Arc::new(filesystem_skill_factory(config_root))))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            protocol::StaticExtensionRegistration::default(),
            vec![
                Arc::new(|| {
                    Ok(Arc::new(AvailableCommandModule {
                        id: "first",
                        commands: vec![
                            protocol::ExtensionCommandDefinition {
                                name: "inspect".to_string(),
                                description: Some("Inspect state".to_string()),
                                argument_hint: Some("<target>".to_string()),
                            },
                            protocol::ExtensionCommandDefinition {
                                name: "shared".to_string(),
                                description: Some("First shared".to_string()),
                                argument_hint: None,
                            },
                        ],
                    }) as Arc<dyn ExtensionModule>)
                }),
                Arc::new(|| {
                    Ok(Arc::new(AvailableCommandModule {
                        id: "second",
                        commands: vec![protocol::ExtensionCommandDefinition {
                            name: "shared".to_string(),
                            description: Some("Second shared".to_string()),
                            argument_hint: None,
                        }],
                    }) as Arc<dyn ExtensionModule>)
                }),
            ],
        )))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-available-commands").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create Session");

    let commands = kernel
        .available_commands(&session_id)
        .expect("Available Commands");
    assert_eq!(
        commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "compact",
            "name",
            "session",
            "first:inspect",
            "first:shared",
            "inspect",
            "second:shared",
            "skill:review",
            "review",
        ]
    );
    let inspect = commands
        .iter()
        .find(|command| command.name == "inspect")
        .expect("short Extension alias");
    assert_eq!(inspect.description, "Inspect state");
    assert_eq!(inspect.argument_hint.as_deref(), Some("<target>"));
    assert_eq!(inspect.source, SlashCommandSource::Extension);
    let skill = commands
        .iter()
        .find(|command| command.name == "skill:review")
        .expect("Skill command");
    assert_eq!(skill.description, "Review Skill");
    assert_eq!(skill.argument_hint.as_deref(), Some("[arguments]"));
    let template = commands
        .iter()
        .find(|command| command.name == "review")
        .expect("Template command");
    assert_eq!(template.argument_hint.as_deref(), Some("<file>"));
}

/// Active Sessions freeze Prompt resources while resume builds a fresh snapshot.
#[tokio::test]
async fn prompt_snapshot_is_stable_until_session_resume() {
    let root = tempfile::tempdir().expect("store root");
    let config_root = root.path().join("config");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&config_root).expect("create config root");
    fs::create_dir_all(&cwd).expect("create project cwd");
    let instruction_path = config_root.join("AGENTS.md");
    fs::write(&instruction_path, "INITIAL PRIVATE INSTRUCTION")
        .expect("write initial instruction");
    let model = Arc::new(PromptCaptureModel(Mutex::new(Vec::new())));
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(7_000)));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(
            Arc::clone(&model) as Arc<dyn Model>
        )))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            config_root,
            protocol::PromptPolicy::default(),
        )))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-prompt-snapshot").expect("session id");
    let session_path = kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create Session");
    fs::write(&instruction_path, "UPDATED PRIVATE INSTRUCTION")
        .expect("update instruction");
    kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "first".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("run active Session");
    kernel
        .close_session(&session_id)
        .await
        .expect("close Session");
    kernel
        .resume_session(session_id.clone(), cwd)
        .await
        .expect("resume Session");
    kernel
        .run(
            RunRequest {
                session_id,
                input: "second".into(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .expect("run resumed Session");

    let requests = model.0.lock().expect("request lock");
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
    assert!(system_prompts[0].contains("INITIAL PRIVATE INSTRUCTION"));
    assert!(!system_prompts[0].contains("UPDATED PRIVATE INSTRUCTION"));
    assert!(system_prompts[1].contains("UPDATED PRIVATE INSTRUCTION"));
    let persisted = fs::read_to_string(session_path).expect("read Session");
    assert!(!persisted.contains("INITIAL PRIVATE INSTRUCTION"));
    assert!(!persisted.contains("UPDATED PRIVATE INSTRUCTION"));
}

/// Follow-up records survive runtime release until explicitly cancelled by QueueId.
#[tokio::test]
async fn queued_follow_up_survives_close_and_resume_until_removed() {
    let root = tempfile::tempdir().expect("store root");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(10_000)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIds(AtomicU64::new(0)));
    let entered = Arc::new(Notify::new());
    let model: Arc<dyn Model> = Arc::new(PendingModel {
        entered: Arc::clone(&entered),
    });
    let kernel =
        build_kernel(root.path().to_path_buf(), model, clock, ids, None);
    let session_id = SessionId::try_from("session-queue").expect("session id");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");

    let running_kernel = Arc::clone(&kernel);
    let running_session = session_id.clone();
    let run = tokio::spawn(async move {
        running_kernel
            .run(
                RunRequest {
                    session_id: running_session,
                    input: "hold".into(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
    });
    entered.notified().await;

    let image_blocks = vec![
        ContentBlock::Text {
            text: "next".to_string(),
        },
        ContentBlock::Image {
            data: "iVBORw0KGgo=".to_string(),
            mime_type: "image/png".to_string(),
        },
    ];
    let queued = kernel
        .queue_message(
            &session_id,
            QueueKind::FollowUp,
            RunInput::Blocks(image_blocks.clone()),
        )
        .await
        .expect("queue");
    assert!(!queued.message.identity.turn_id.as_str().is_empty());
    assert_eq!(
        queued.message.content,
        MessageContent::User {
            blocks: image_blocks
        }
    );
    kernel.cancel_session(&session_id).expect("cancel");
    run.await.expect("join").expect("settle");
    kernel
        .close_session(&session_id)
        .await
        .expect("close session");
    kernel
        .resume_session(session_id.clone(), cwd)
        .await
        .expect("resume session");

    assert_eq!(
        kernel
            .pending_messages(&session_id)
            .expect("pending")
            .follow_up,
        vec![queued.clone()]
    );
    kernel
        .remove_pending_message(&session_id, &queued.queue_id)
        .expect("remove");
    assert!(
        kernel
            .pending_messages(&session_id)
            .expect("pending")
            .follow_up
            .is_empty()
    );
}

/// A follow-up racing final settlement is consumed once and never revives on Resume.
#[tokio::test]
async fn queue_racing_final_checkpoint_recovers_exactly_once() {
    let root = tempfile::tempdir().expect("store root");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(12_000)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIds(AtomicU64::new(0)));
    let model = Arc::new(GatedCaptureModel::default());
    let kernel = build_kernel(
        root.path().to_path_buf(),
        Arc::clone(&model) as Arc<dyn Model>,
        clock,
        ids,
        None,
    );
    let session_id =
        SessionId::try_from("session-image-follow-up").expect("session id");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            parent_session_id: None,
        })
        .await
        .expect("create session");

    let running_kernel = Arc::clone(&kernel);
    let running_session = session_id.clone();
    let run = tokio::spawn(async move {
        running_kernel
            .run(
                RunRequest {
                    session_id: running_session,
                    input: "first".into(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
    });
    model.first_entered.notified().await;

    let image_blocks = vec![
        ContentBlock::Text {
            text: "describe this image".to_string(),
        },
        ContentBlock::Image {
            data: "iVBORw0KGgo=".to_string(),
            mime_type: "image/png".to_string(),
        },
    ];
    let queued = kernel
        .queue_message(
            &session_id,
            QueueKind::FollowUp,
            RunInput::Blocks(image_blocks.clone()),
        )
        .await
        .expect("queue image follow-up");
    model.first_release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .expect("queued follow-up run timeout")
        .expect("join queued follow-up run")
        .expect("complete queued follow-up run");

    assert!(
        kernel
            .pending_messages(&session_id)
            .expect("pending messages")
            .follow_up
            .is_empty()
    );
    kernel
        .close_session(&session_id)
        .await
        .expect("close after queue consumption");
    kernel
        .resume_session(session_id.clone(), cwd)
        .await
        .expect("resume after queue consumption");
    assert!(
        kernel
            .pending_messages(&session_id)
            .expect("resumed pending messages")
            .follow_up
            .is_empty()
    );
    let queued_message_count = kernel
        .session_transcript(&session_id)
        .expect("resumed transcript")
        .into_iter()
        .filter(|message| {
            message.identity.message_id == queued.message.identity.message_id
        })
        .count();
    assert_eq!(queued_message_count, 1);
    let requests = model.requests.lock().expect("request lock");
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].messages.iter().any(|message| {
            matches!(
                &message.content,
                MessageContent::User { blocks } if blocks == &image_blocks
            )
        }),
        "second request did not contain queued image blocks: {:#?}",
        requests[1].messages
    );
}

/// Steer and Follow-up queue entries share Prompt expansion and reject commands.
#[tokio::test]
async fn queued_messages_expand_session_resources_before_persistence() {
    let root = tempfile::tempdir().expect("store root");
    let config_root = root.path().join("config");
    let cwd = root.path().join("workspace");
    let skill = config_root.join("skills/queued/SKILL.md");
    let template = config_root.join("prompts/queued.md");
    fs::create_dir_all(skill.parent().expect("Skill parent"))
        .expect("create Skill parent");
    fs::create_dir_all(template.parent().expect("Template parent"))
        .expect("create Template parent");
    fs::create_dir_all(&cwd).expect("create project cwd");
    fs::write(
        &skill,
        "---\nname: queued\ndescription: Queued Skill\n---\nQUEUED SKILL\n",
    )
    .expect("write Skill");
    fs::write(&template, "QUEUED TEMPLATE: $ARGUMENTS")
        .expect("write Template");
    let entered = Arc::new(Notify::new());
    let model: Arc<dyn Model> = Arc::new(PendingModel {
        entered: Arc::clone(&entered),
    });
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(15_000)));
    let kernel = Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(StaticModelFactory(model)))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root.path(),
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                config_root.clone(),
                protocol::PromptPolicy::default(),
            )))
            .skill_factory(Some(Arc::new(filesystem_skill_factory(
                config_root,
            ))))
            .extension_factory(Arc::new(StaticExtensionFactory::new(
                protocol::StaticExtensionRegistration::default(),
                vec![Arc::new(|| {
                    Ok(Arc::new(QueueCommandModule) as Arc<dyn ExtensionModule>)
                })],
            )))
            .clock(clock)
            .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
            .build()
            .build()
            .expect("build kernel"),
    );
    let session_id =
        SessionId::try_from("session-queue-expansion").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create Session");
    let running_kernel = Arc::clone(&kernel);
    let running_session = session_id.clone();
    let run = tokio::spawn(async move {
        running_kernel
            .run(
                RunRequest {
                    session_id: running_session,
                    input: "hold".into(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
    });
    entered.notified().await;

    let steering = kernel
        .queue_message(
            &session_id,
            QueueKind::Steering,
            "/skill:queued alpha".to_string(),
        )
        .await
        .expect("queue expanded Skill");
    let follow_up = kernel
        .queue_message(
            &session_id,
            QueueKind::FollowUp,
            "/queued beta".to_string(),
        )
        .await
        .expect("queue expanded Template");
    let error = kernel
        .queue_message(
            &session_id,
            QueueKind::FollowUp,
            "/not-queueable later".to_string(),
        )
        .await
        .expect_err("Extension command cannot be queued");
    assert!(matches!(
        error,
        kernel::KernelError::SlashCommandCannotQueue {
            name,
            command_source,
        }
            if name == "not-queueable"
                && command_source == protocol::SlashCommandSource::Extension
    ));
    let MessageContent::ExpandedUser {
        expansion: steering_expansion,
    } = &steering.message.content
    else {
        panic!("Steering expanded User message expected");
    };
    assert!(
        steering_expansion.model_blocks[0]
            .text()
            .is_some_and(|text| text.contains("QUEUED SKILL"))
    );
    assert!(
        steering_expansion.model_blocks[0]
            .text()
            .is_some_and(|text| text.ends_with("alpha"))
    );
    let MessageContent::ExpandedUser {
        expansion: follow_up_expansion,
    } = &follow_up.message.content
    else {
        panic!("Follow-up expanded User message expected");
    };
    assert_eq!(
        follow_up_expansion.model_blocks[0].text(),
        Some("QUEUED TEMPLATE: beta")
    );
    assert_eq!(
        kernel
            .pending_messages(&session_id)
            .expect("pending queue")
            .steering,
        vec![steering]
    );
    kernel
        .cancel_session(&session_id)
        .expect("cancel active Run");
    run.await.expect("join Run").expect("settle Run");
}

/// First user content creates a title event while a later manual name remains authoritative.
#[tokio::test]
async fn first_user_message_sets_default_title_and_manual_rename_wins() {
    let root = tempfile::tempdir().expect("store root");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(20_000)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIds(AtomicU64::new(0)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::from([vec![
            ModelStreamEvent::TextDelta("ok".to_string()),
            ScriptedModel::finished(StopReason::EndTurn),
        ]])),
    });
    let kernel =
        build_kernel(root.path().to_path_buf(), model, clock, ids, None);
    let session_id = SessionId::try_from("session-title").expect("session id");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let sink = Arc::new(RecordingSink::default());

    kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "  First line\nsecond line".into(),
            },
            sink.clone(),
        )
        .await
        .expect("run");
    assert_eq!(
        kernel
            .session_tree(&session_id)
            .expect("tree")
            .name
            .as_deref(),
        Some("First line")
    );
    let tree_wire = serde_json::to_value(
        kernel
            .session_tree(&session_id)
            .expect("tree wire snapshot"),
    )
    .expect("serialize tree snapshot");
    assert_eq!(tree_wire["sessionId"], "session-title");
    assert!(tree_wire.get("session_id").is_none());
    assert!(tree_wire["entries"][0].get("entryId").is_some());
    assert!(sink.0.lock().expect("sink lock").iter().any(|event| {
        matches!(
            &event.payload,
            AgentEventPayload::SessionTitleChanged { title }
                if title == "First line"
        )
    }));

    kernel
        .rename_session(
            &session_id,
            SessionTitle::try_from("Manual").expect("title"),
        )
        .await
        .expect("rename");
    assert_eq!(
        kernel
            .session_tree(&session_id)
            .expect("tree")
            .name
            .as_deref(),
        Some("Manual")
    );
    assert_eq!(
        kernel.list_sessions(None).expect("list")[0].name.as_deref(),
        Some("Manual")
    );
}

/// Skill listing exposes effective metadata but leaves source retrieval explicit.
#[tokio::test]
async fn skills_return_metadata_without_file_bodies() {
    let root = tempfile::tempdir().expect("store root");
    let skill_root = tempfile::tempdir().expect("skill root");
    let skill_path = skill_root.path().join("skills/review/SKILL.md");
    fs::create_dir_all(skill_path.parent().expect("Skill parent"))
        .expect("create Skill parent");
    fs::write(
        &skill_path,
        "---\nname: review\ndescription: Review code\n---\nSECRET BODY\n",
    )
    .expect("write skill");
    let skill_path = fs::canonicalize(skill_path).expect("canonical Skill");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(30_000)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIds(AtomicU64::new(0)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    let kernel = build_kernel(
        root.path().to_path_buf(),
        model,
        clock,
        ids,
        Some(skill_root.path().to_path_buf()),
    );
    let session_id = SessionId::try_from("session-skills").expect("session id");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");

    let skills = kernel.skills(&session_id).expect("Session Skills");
    assert_eq!(skills.skills.len(), 1);
    assert_eq!(skills.skills[0].name, "review");
    assert_eq!(skills.skills[0].description, "Review code");
    assert_eq!(skills.skills[0].path, skill_path);
    assert!(
        !serde_json::to_string(&skills)
            .expect("serialize")
            .contains("SECRET")
    );
    assert!(
        kernel
            .invoke_skill(&session_id, "review")
            .expect("invoke Session Skill")
            .contains("SECRET BODY")
    );
}

/// Queued Skill diagnostics use the Turn active after synchronous expansion completes.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_skill_diagnostic_uses_latest_active_turn() {
    let root = tempfile::tempdir().expect("store root");
    let skill_root = tempfile::tempdir().expect("skill root");
    let skill_path = skill_root.path().join("skills/review/SKILL.md");
    fs::create_dir_all(skill_path.parent().expect("Skill parent"))
        .expect("create Skill parent");
    fs::write(
        &skill_path,
        "---\nname: review\ndescription: Review code\n---\nreview body\n",
    )
    .expect("write Skill");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(34_000)));
    let model = Arc::new(TwoTurnGatedModel::default());
    let kernel = build_kernel(
        root.path().to_path_buf(),
        Arc::clone(&model) as Arc<dyn Model>,
        clock,
        Arc::new(SequentialIds(AtomicU64::new(0))),
        Some(skill_root.path().to_path_buf()),
    );
    let session_id =
        SessionId::try_from("session-queued-skill-race").expect("session id");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    fs::remove_file(&skill_path).expect("remove discovered Skill file");
    let status = std::process::Command::new("mkfifo")
        .arg(&skill_path)
        .status()
        .expect("execute mkfifo");
    assert!(
        status.success(),
        "mkfifo must create the blocking Skill path"
    );
    let sink = Arc::new(RecordingSink::default());

    let running_kernel = Arc::clone(&kernel);
    let running_session = session_id.clone();
    let running_sink = Arc::clone(&sink);
    let run = tokio::spawn(async move {
        running_kernel
            .run(
                RunRequest {
                    session_id: running_session,
                    input: "first".into(),
                },
                running_sink as Arc<dyn EventSink>,
            )
            .await
    });
    model.entered[0].notified().await;
    kernel
        .queue_message(&session_id, QueueKind::FollowUp, "advance")
        .await
        .expect("queue second Turn");

    let (writer_opened_tx, writer_opened_rx) = tokio::sync::oneshot::channel();
    let (write_tx, write_rx) = std::sync::mpsc::sync_channel(1);
    let writer_path = skill_path.clone();
    let writer = std::thread::spawn(move || {
        let mut fifo = std::fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .expect("open Skill FIFO writer");
        writer_opened_tx.send(()).expect("signal FIFO connection");
        write_rx.recv().expect("wait to complete Skill read");
        std::io::Write::write_all(&mut fifo, &[0xff])
            .expect("write invalid UTF-8");
    });
    let queued_kernel = Arc::clone(&kernel);
    let queued_session = session_id.clone();
    let queued = tokio::spawn(async move {
        queued_kernel
            .queue_message(
                &queued_session,
                QueueKind::FollowUp,
                "/skill:review inspect this",
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), writer_opened_rx)
        .await
        .expect("Skill expansion must reach the FIFO")
        .expect("FIFO writer signal");

    model.releases[0].notify_one();
    tokio::time::timeout(Duration::from_secs(2), model.entered[1].notified())
        .await
        .expect("run must advance to the queued Turn");
    write_tx.send(()).expect("complete Skill read");
    tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("diagnostic queue timeout")
        .expect("join diagnostic queue")
        .expect("queue diagnostic message");
    writer.join().expect("join FIFO writer");

    {
        let events = sink.0.lock().expect("event lock");
        let turn_ids = events
            .iter()
            .filter(|event| {
                matches!(event.payload, AgentEventPayload::TurnStart { .. })
            })
            .map(|event| event.metadata.turn_id.clone())
            .collect::<Vec<_>>();
        let diagnostic_turn = events.iter().find_map(|event| {
            matches!(event.payload, AgentEventPayload::SkillDiagnostic { .. })
                .then(|| event.metadata.turn_id.clone())
        });
        assert_eq!(turn_ids.len(), 2);
        assert_eq!(diagnostic_turn.as_ref(), turn_ids.get(1));
        assert_ne!(diagnostic_turn.as_ref(), turn_ids.first());
    }

    model.releases[1].notify_one();
    tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("run completion timeout")
        .expect("join run")
        .expect("complete run");
}

/// Automatic Skill read failures preserve input and emit a Turn-correlated diagnostic.
#[tokio::test]
async fn skill_expansion_failure_preserves_input_and_emits_diagnostic() {
    let root = tempfile::tempdir().expect("store root");
    let skill_root = tempfile::tempdir().expect("skill root");
    let skill_path = skill_root.path().join("skills/review/SKILL.md");
    fs::create_dir_all(skill_path.parent().expect("Skill parent"))
        .expect("create Skill parent");
    fs::write(
        &skill_path,
        "---\nname: review\ndescription: Review code\n---\nreview body\n",
    )
    .expect("write Skill");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(35_000)));
    let model = Arc::new(PromptCaptureModel(Mutex::new(Vec::new())));
    let kernel = build_kernel(
        root.path().to_path_buf(),
        Arc::clone(&model) as Arc<dyn Model>,
        clock,
        Arc::new(SequentialIds(AtomicU64::new(0))),
        Some(skill_root.path().to_path_buf()),
    );
    let session_id =
        SessionId::try_from("session-skill-read-failure").expect("session id");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    fs::remove_file(&skill_path).expect("remove Skill after discovery");
    let sink = Arc::new(RecordingSink::default());

    kernel
        .run(
            RunRequest {
                session_id,
                input: "/skill:review inspect this".into(),
            },
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .await
        .expect("run with preserved Skill input");

    let requests = model.0.lock().expect("request lock");
    assert!(requests[0].messages.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::User { blocks }
                if blocks.iter().any(|block| {
                    block.text() == Some("/skill:review inspect this")
                })
        )
    }));
    let events = sink.0.lock().expect("event lock");
    let diagnostic = events.iter().find_map(|event| match &event.payload {
        AgentEventPayload::SkillDiagnostic { diagnostic } => Some((
            &event.metadata.turn_id,
            &event.metadata.timestamp_ms,
            diagnostic,
        )),
        _ => None,
    });
    let (turn_id, timestamp_ms, diagnostic) =
        diagnostic.expect("Skill diagnostic event");
    assert!(!turn_id.as_ref().is_empty());
    assert!(timestamp_ms.get() > 0);
    assert_eq!(diagnostic.code, SkillDiagnosticCode::FileReadFailed);
}

/// Kernel retains the session factory's MCP status without reconnecting on reads.
#[tokio::test]
async fn mcp_status_is_available_from_the_registered_session() {
    let root = tempfile::tempdir().expect("store root");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(40_000)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(StaticModelFactory(model)))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::default()))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
        .mcp_factory(Some(Arc::new(StaticMcpFactory)))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .build()
        .build()
        .expect("build kernel");
    let session_id = SessionId::try_from("session-mcp").expect("session id");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");

    let status = kernel.mcp_status(&session_id).expect("MCP status");
    assert_eq!(status.servers.len(), 1);
    assert_eq!(status.servers[0].server_id.as_str(), "docs");
    assert_eq!(status.servers[0].state, McpServerState::Disabled);
}

/// Deleting an active session releases its runtime and removes persistent history.
#[tokio::test]
async fn delete_session_removes_active_and_persisted_state_idempotently() {
    let root = tempfile::tempdir().expect("store root");
    let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(50_000)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIds(AtomicU64::new(0)));
    let model: Arc<dyn Model> = Arc::new(ScriptedModel {
        scripts: Mutex::new(VecDeque::new()),
    });
    let kernel =
        build_kernel(root.path().to_path_buf(), model, clock, ids, None);
    let session_id = SessionId::try_from("session-delete").expect("session id");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&cwd).expect("create project cwd");
    let path = kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");

    kernel
        .delete_session(&session_id)
        .await
        .expect("delete active session");

    assert!(!path.exists());
    assert!(
        kernel
            .list_sessions(None)
            .expect("list sessions")
            .is_empty()
    );
    assert!(matches!(
        kernel.session_tree(&session_id),
        Err(kernel::KernelError::SessionNotFound(_))
    ));
    kernel
        .delete_session(&session_id)
        .await
        .expect("repeat delete");
}
