use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use extension::{
    ExtensionCommandContext, ExtensionCommandHandler, ExtensionContext,
    ExtensionError, ExtensionHandler, ExtensionHostError, ExtensionModule,
    ExtensionRegistrar, InputPoint, SessionBeforeTreePoint,
    SessionShutdownPoint, SessionStartPoint, StaticExtensionFactory,
    UserBashPoint,
};
use futures::{Stream, stream};
use kernel::{
    EventSink, KernelFactory, Model, ModelCatalog, ModelError, ModelFactory,
};
use protocol::{
    AgentEvent, ContentBlock, ExtensionCommandDefinition, ExtensionDescriptor,
    ExtensionEntryData, ExtensionEventData, ExtensionFlagDefinition,
    ExtensionFlagKind, ExtensionId, InputEvent, InputResult, MessageContent,
    ModelFinal, ModelProfile, ModelRequest, ModelStreamEvent, ModelUsage,
    RunRequest, SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionId,
    SessionShutdownEvent, SessionStartEvent, StaticExtensionRegistration,
    StopReason, ToolCall, ToolDefinition, ToolResult, TreeSummary,
    UserBashDisposition, UserBashEvent, UserBashResult,
};
use store::{JsonlStoreFactory, SessionCreateOptions, SystemClock};
use tokio_util::sync::CancellationToken;
use tools::{AgentTool, BuiltinToolFactory, ToolError, ToolExecutionContext};

struct HostModel {
    profile: ModelProfile,
}

#[async_trait]
impl Model for HostModel {
    /// Returns the deterministic profile used by host-action tests.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Confirms that the local fixture requires no provider readiness.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Produces one complete response when a test elects to run the agent.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<
        Pin<
            Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>,
        >,
        ModelError,
    > {
        Ok(Box::pin(stream::iter([Ok(ModelStreamEvent::Finished(
            ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: None,
                usage: ModelUsage::builder()
                    .input_tokens(0)
                    .output_tokens(0)
                    .cache_read_tokens(0)
                    .cache_write_tokens(0)
                    .total_tokens(0)
                    .build(),
            },
        ))])))
    }
}

struct HostModelFactory;

impl ModelFactory for HostModelFactory {
    /// Creates a fresh local model without external provider access.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(HostModel {
            profile: ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id("host".to_string())
                .display_name("Host fixture".to_string())
                .context_tokens(128_000)
                .max_output_tokens(1_024)
                .build(),
        }))
    }
}

/// Model factory that records the complete static registration passed by the kernel.
struct ObservingModelFactory {
    registration: Arc<Mutex<Option<StaticExtensionRegistration>>>,
}

impl ModelFactory for ObservingModelFactory {
    /// Creates the same deterministic model used by host tests.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        HostModelFactory.create()
    }

    /// Records static declarations before returning the model catalog.
    fn create_catalog(
        &self,
        registration: &StaticExtensionRegistration,
    ) -> Result<ModelCatalog, ModelError> {
        *self.registration.lock().expect("registration lock") =
            Some(registration.clone());
        Ok(ModelCatalog::single(self.create()?))
    }
}

struct HostCommand;

struct HostTreeSummary;

#[async_trait]
impl ExtensionHandler<SessionBeforeTreePoint> for HostTreeSummary {
    /// Supplies a deterministic branch summary without invoking the model.
    async fn handle(
        &self,
        _event: &SessionBeforeTreeEvent,
        _context: &ExtensionContext,
    ) -> Result<SessionBeforeTreeResult, ExtensionError> {
        Ok(SessionBeforeTreeResult::builder()
            .summary(Some(TreeSummary {
                summary: "extension branch summary".to_string(),
                details: Some(serde_json::json!({ "source": "test" })),
                usage: None,
            }))
            .build())
    }
}

struct DiscardSink;

#[async_trait]
impl EventSink for DiscardSink {
    /// Accepts events while the test inspects persisted state directly.
    async fn emit(&self, _event: AgentEvent) -> Result<(), kernel::SinkError> {
        Ok(())
    }
}

#[async_trait]
impl ExtensionCommandHandler for HostCommand {
    /// Exercises persisted, session-creation, and application-shutdown host actions.
    async fn handle(
        &self,
        _arguments: &str,
        _parameters: &serde_json::Value,
        context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        let prompt = context
            .event
            .system_prompt()
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        if prompt.trim().is_empty() {
            return Err(ExtensionError::Handler(
                "system prompt is empty".to_string(),
            ));
        }
        let mut events = context
            .event
            .subscribe_events()
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        context
            .event
            .publish_event(ExtensionEventData {
                channel: "host".to_string(),
                value: serde_json::json!({ "ready": true }),
            })
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        let event = events
            .recv()
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        if event.channel != "host" {
            return Err(ExtensionError::Handler(
                "unexpected event channel".to_string(),
            ));
        }
        context
            .event
            .send_extension_message(
                protocol::ExtensionMessageDraft::builder()
                    .extension_id(
                        ExtensionId::try_from("host").expect("extension id"),
                    )
                    .custom_type("notice".to_string())
                    .blocks(vec![ContentBlock::Text {
                        text: "out of turn".to_string(),
                    }])
                    .display(true)
                    .include_in_context(false)
                    .build(),
            )
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        context
            .event
            .set_session_name(Some("Managed by extension".to_string()))
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        context
            .event
            .append_entry(ExtensionEntryData {
                extension_id: ExtensionId::try_from("host")
                    .expect("extension id"),
                custom_type: "audit".to_string(),
                data: serde_json::json!({ "ok": true }),
            })
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        context
            .create_session(context.event.invocation.cwd.join("child"))
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        context
            .event
            .shutdown()
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))
    }
}

struct HostModule;

impl ExtensionModule for HostModule {
    /// Declares the extension identity used to validate authored entries.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("host").expect("extension id"),
            name: "Host actions".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the command that exercises the Kernel-backed host.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.register_command(
            ExtensionCommandDefinition {
                name: "manage".to_string(),
                description: None,
            },
            HostCommand,
        )?;
        registrar.on::<SessionBeforeTreePoint, _>(HostTreeSummary)
    }
}

struct FailingInput;

#[async_trait]
impl ExtensionHandler<InputPoint> for FailingInput {
    /// Produces the handler failure whose durable diagnostic is under test.
    async fn handle(
        &self,
        _event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        Err(ExtensionError::Handler(
            "input rejected by fixture".to_string(),
        ))
    }
}

struct FailingModule;

impl ExtensionModule for FailingModule {
    /// Declares the extension identity persisted with handler diagnostics.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("failing").expect("extension id"),
            name: "Failing input".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers one input handler that always fails without blocking the run.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<InputPoint, _>(FailingInput)
    }
}

struct ReplacingUserBash;

#[async_trait]
impl ExtensionHandler<UserBashPoint> for ReplacingUserBash {
    /// Replaces one sentinel command while allowing all other commands to use the local executor.
    async fn handle(
        &self,
        event: &UserBashEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<UserBashResult>, ExtensionError> {
        Ok((event.command == "handled").then(|| {
            UserBashResult::builder()
                .disposition(UserBashDisposition::Completed)
                .output("extension output".to_string())
                .exit_code(Some(23))
                .cancelled(false)
                .truncated(false)
                .build()
        }))
    }
}

struct UserBashModule;

impl ExtensionModule for UserBashModule {
    /// Declares the extension that replaces the sentinel user-bash command.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("user-bash").expect("extension id"),
            name: "User bash replacement".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the server-side user-bash replacement hook.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<UserBashPoint, _>(ReplacingUserBash)
    }
}

struct ToolCaptureModel {
    profile: ModelProfile,
    requests: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl Model for ToolCaptureModel {
    /// Returns the deterministic model profile used by dynamic-tool tests.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Confirms that tool capture requires no provider readiness.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Records the exact tool definitions supplied to one model request.
    async fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<
        Pin<
            Box<dyn Stream<Item = Result<ModelStreamEvent, ModelError>> + Send>,
        >,
        ModelError,
    > {
        self.requests
            .lock()
            .expect("tool request lock")
            .push(request.tools.into_iter().map(|tool| tool.name).collect());
        Ok(Box::pin(stream::iter([Ok(ModelStreamEvent::Finished(
            ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: None,
                usage: ModelUsage::builder()
                    .input_tokens(0)
                    .output_tokens(0)
                    .cache_read_tokens(0)
                    .cache_write_tokens(0)
                    .total_tokens(0)
                    .build(),
            },
        ))])))
    }
}

struct ToolCaptureModelFactory {
    requests: Arc<Mutex<Vec<Vec<String>>>>,
}

impl ModelFactory for ToolCaptureModelFactory {
    /// Creates a model that records each request's tool names.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(ToolCaptureModel {
            profile: ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id("tool-capture".to_string())
                .display_name("Tool capture".to_string())
                .context_tokens(128_000)
                .max_output_tokens(1_024)
                .build(),
            requests: Arc::clone(&self.requests),
        }))
    }
}

struct SessionDynamicTool;

#[async_trait]
impl AgentTool for SessionDynamicTool {
    /// Describes the tool dynamically added by a session-start handler.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "session_dynamic".to_string(),
            description: "Session-local dynamic tool.".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    /// Returns a deterministic result if the model elects to invoke this fixture.
    async fn execute(
        &self,
        call: ToolCall,
        _context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text {
                text: "dynamic".to_string(),
            }])
            .is_error(false)
            .build())
    }
}

struct SessionDynamicToolRegistration;

#[async_trait]
impl ExtensionHandler<SessionStartPoint> for SessionDynamicToolRegistration {
    /// Registers the tool only for the session rooted at the enabled directory.
    async fn handle(
        &self,
        _event: &SessionStartEvent,
        context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        if context.invocation.cwd.ends_with("enabled") {
            context
                .register_tool(Arc::new(SessionDynamicTool))
                .await
                .map_err(|error| ExtensionError::Handler(error.to_string()))?;
        }
        Ok(())
    }
}

struct SessionDynamicToolModule;

impl ExtensionModule for SessionDynamicToolModule {
    /// Declares the extension that owns the dynamic session tool.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("dynamic-tool").expect("extension id"),
            name: "Dynamic tool".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the session-start tool installation handler.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<SessionStartPoint, _>(SessionDynamicToolRegistration)
    }
}

struct ShutdownReentry {
    errors: Arc<Mutex<Vec<ExtensionHostError>>>,
}

#[async_trait]
impl ExtensionHandler<SessionShutdownPoint> for ShutdownReentry {
    /// Records host operations that must be rejected instead of reentering the run gate.
    async fn handle(
        &self,
        _event: &SessionShutdownEvent,
        context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        let send_error = context
            .send_user_message(protocol::ExtensionUserMessage {
                text: "must not run while closing".to_string(),
                delivery: protocol::QueueKind::FollowUp,
                parent_entry_id: None,
            })
            .await
            .expect_err("shutdown user message must be rejected");
        let compact_error = context
            .compact()
            .await
            .expect_err("shutdown compaction must be rejected");
        self.errors
            .lock()
            .expect("shutdown error lock")
            .extend([send_error, compact_error]);
        Ok(())
    }
}

struct ShutdownReentryModule {
    errors: Arc<Mutex<Vec<ExtensionHostError>>>,
}

impl ExtensionModule for ShutdownReentryModule {
    /// Declares the extension used to exercise shutdown host reentry.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("shutdown-reentry")
                .expect("extension id"),
            name: "Shutdown reentry".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers one shutdown handler that calls run-gated host operations.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<SessionShutdownPoint, _>(ShutdownReentry {
            errors: Arc::clone(&self.errors),
        })
    }
}

/// Kernel construction applies static declarations before model-catalog creation.
#[test]
fn static_extensions_are_frozen_before_models_and_remain_queryable() {
    let root = tempfile::tempdir().expect("store root");
    let observed = Arc::new(Mutex::new(None));
    let flag = ExtensionFlagDefinition::builder()
        .name("audit".to_string())
        .kind(ExtensionFlagKind::Boolean)
        .default(Some(serde_json::json!(true)))
        .build();
    let registration = StaticExtensionRegistration::builder()
        .flags(vec![flag.clone()])
        .flag_values(serde_json::Map::from_iter([(
            "audit".to_string(),
            serde_json::json!(true),
        )]))
        .build();

    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(ObservingModelFactory {
            registration: Arc::clone(&observed),
        }))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::new(SystemClock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            registration.clone(),
            Vec::new(),
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(kernel::NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");

    assert_eq!(
        observed
            .lock()
            .expect("registration lock")
            .as_ref()
            .expect("model factory registration"),
        &registration
    );
    assert_eq!(kernel.extension_flags(), vec![flag]);
    assert_eq!(
        kernel.extension_flag_values().get("audit"),
        Some(&serde_json::json!(true))
    );
}

/// Shutdown hooks reject run-gated host actions without deadlocking session close.
#[tokio::test]
async fn shutdown_reentrant_operations_are_rejected() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let errors = Arc::new(Mutex::new(Vec::new()));
    let module_errors = Arc::clone(&errors);
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(HostModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(ShutdownReentryModule {
                    errors: Arc::clone(&module_errors),
                }) as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(kernel::NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("shutdown-session").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");

    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        kernel.close_session(&session_id),
    )
    .await
    .expect("session close must not deadlock")
    .expect("close session");

    assert_eq!(
        *errors.lock().expect("shutdown error lock"),
        vec![
            ExtensionHostError::SessionClosing,
            ExtensionHostError::SessionClosing,
        ]
    );
}

/// A dynamically registered tool becomes active only in its owning session.
#[tokio::test]
async fn dynamically_registered_tool_is_active_in_its_session() {
    let root = tempfile::tempdir().expect("store root");
    let enabled_cwd = root.path().join("enabled");
    let disabled_cwd = root.path().join("disabled");
    std::fs::create_dir_all(&enabled_cwd).expect("create enabled workspace");
    std::fs::create_dir_all(&disabled_cwd).expect("create disabled workspace");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(ToolCaptureModelFactory {
            requests: Arc::clone(&requests),
        }))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(|| {
                Ok(Arc::new(SessionDynamicToolModule)
                    as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(kernel::NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let enabled_session =
        SessionId::try_from("dynamic-enabled").expect("session id");
    let disabled_session =
        SessionId::try_from("dynamic-disabled").expect("session id");
    for (session_id, cwd) in [
        (enabled_session.clone(), enabled_cwd),
        (disabled_session.clone(), disabled_cwd),
    ] {
        kernel
            .create_session(SessionCreateOptions {
                session_id,
                cwd,
                parent_session_id: None,
            })
            .await
            .expect("create session");
    }

    for session_id in [enabled_session, disabled_session] {
        kernel
            .run(
                RunRequest {
                    session_id,
                    input: "inspect tools".to_string(),
                },
                Arc::new(DiscardSink),
            )
            .await
            .expect("run agent");
    }

    let requests = requests.lock().expect("tool request lock");
    assert!(requests[0].iter().any(|name| name == "session_dynamic"));
    assert!(!requests[1].iter().any(|name| name == "session_dynamic"));
}

/// Kernel-backed command actions persist data, create sessions, and signal shutdown.
#[tokio::test]
async fn command_context_uses_complete_kernel_host() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    std::fs::create_dir_all(cwd.join("child")).expect("create child workspace");
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(HostModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(|| {
                Ok(Arc::new(HostModule) as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(kernel::NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id = SessionId::try_from("host-session").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");

    kernel
        .invoke_extension_command(
            &session_id,
            "host/manage".to_string(),
            serde_json::Value::Null,
        )
        .await
        .expect("invoke host command");

    let tree = kernel.session_tree(&session_id).expect("session tree");
    assert_eq!(tree.name.as_deref(), Some("Managed by extension"));
    assert!(tree.entries.iter().any(|entry| {
        entry.kind == "custom"
            && entry.payload["extensionId"] == "host"
            && entry.payload["customType"] == "audit"
    }));
    assert!(tree.entries.iter().any(|entry| {
        entry.kind == "message"
            && entry.payload["content"]["type"] == "extension"
            && entry.payload["turn_id"] == "system-host-session"
    }));
    assert_eq!(kernel.list_sessions(None).expect("list sessions").len(), 2);
    tokio::time::timeout(
        std::time::Duration::from_millis(50),
        kernel.wait_for_shutdown(),
    )
    .await
    .expect("shutdown signal");
}

/// Tree navigation persists an extension-provided Pi branch summary at the destination.
#[tokio::test]
async fn navigation_uses_extension_branch_summary() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(HostModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(|| {
                Ok(Arc::new(HostModule) as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(kernel::NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id = SessionId::try_from("tree-session").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    for input in ["first", "second"] {
        kernel
            .run(
                RunRequest {
                    session_id: session_id.clone(),
                    input: input.to_string(),
                },
                Arc::new(DiscardSink),
            )
            .await
            .expect("run agent");
    }
    let target = kernel
        .session_tree(&session_id)
        .expect("tree before navigation")
        .entries
        .into_iter()
        .find(|entry| {
            entry.kind == "message"
                && entry.payload["content"]["type"] == "user"
        })
        .expect("first user entry")
        .entry_id;

    let tree = kernel
        .navigate_session_with_summary(&session_id, target, true)
        .await
        .expect("navigate with summary");
    let summary = tree
        .entries
        .iter()
        .find(|entry| entry.kind == "branch_summary")
        .expect("branch summary entry");
    assert_eq!(summary.payload["summary"], "extension branch summary");
    assert_eq!(summary.payload["fromHook"], true);
    assert_eq!(summary.payload["details"]["source"], "test");
}

/// Handler failures remain visible after the owning session is closed and resumed.
#[tokio::test]
async fn extension_handler_failure_is_persisted_for_replay() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(HostModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(|| {
                Ok(Arc::new(FailingModule) as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(kernel::NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("diagnostic-session").expect("session id");
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
                input: "continue after extension failure".to_string(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("run agent");

    let transcript = kernel
        .session_transcript(&session_id)
        .expect("live transcript");
    assert!(transcript.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::Extension { extension }
                if extension.extension_id.as_ref() == "failing"
                    && extension.custom_type == "input"
                    && extension.display
                    && !extension.include_in_context
                    && extension.details.as_ref().is_some_and(|details| {
                        details["kind"] == "handler_error"
                    })
        )
    }));

    kernel
        .close_session(&session_id)
        .await
        .expect("close session");
    kernel
        .resume_session(session_id.clone(), cwd)
        .await
        .expect("resume session");
    let replay = kernel
        .session_transcript(&session_id)
        .expect("replayed transcript");
    assert!(replay.iter().any(|message| {
        matches!(
            &message.content,
            MessageContent::Extension { extension }
                if extension.details.as_ref().is_some_and(|details| {
                    details["kind"] == "handler_error"
                })
        )
    }));
}

/// Prefixed user input executes on the server, honors replacement hooks, and persists Pi bash messages.
#[tokio::test]
async fn user_bash_uses_hooks_and_preserves_context_exclusion() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(HostModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(|| {
                Ok(Arc::new(UserBashModule) as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(clock)
        .id_generator(Arc::new(kernel::NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("user-bash-session").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");

    for input in [
        "!printf 'default output'",
        "!!printf 'private output'",
        "!handled",
        "!exit 126",
    ] {
        let result = kernel
            .run(
                RunRequest {
                    session_id: session_id.clone(),
                    input: input.to_string(),
                },
                Arc::new(DiscardSink),
            )
            .await
            .expect("execute user bash");
        assert_eq!(result.messages.len(), 1);
        assert!(result.turns.is_empty());
    }

    let transcript = kernel
        .session_transcript(&session_id)
        .expect("user bash transcript");
    assert!(matches!(
        &transcript[0].content,
        MessageContent::BashExecution { bash }
            if bash.command == "printf 'default output'"
                && bash.result.output == "default output"
                && !bash.exclude_from_context
    ));
    assert!(matches!(
        &transcript[1].content,
        MessageContent::BashExecution { bash }
            if bash.command == "printf 'private output'"
                && bash.result.output == "private output"
                && bash.exclude_from_context
    ));
    assert!(matches!(
        &transcript[2].content,
        MessageContent::BashExecution { bash }
            if bash.command == "handled"
                && bash.result.output == "extension output"
                && bash.result.exit_code == Some(23)
    ));
    assert!(matches!(
        &transcript[3].content,
        MessageContent::BashExecution { bash }
            if bash.result.exit_code == Some(126)
                && bash.result.disposition == UserBashDisposition::Completed
    ));
    assert!(transcript.iter().all(|message| {
        !message.identity.turn_id.as_str().is_empty()
            && message.timing.started_at_ms <= message.timing.ended_at_ms
    }));
}
