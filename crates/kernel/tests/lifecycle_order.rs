use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use extension::{
    AgentEndPoint, AgentSettledPoint, AgentStartPoint, BeforeAgentStartPoint,
    ExtensionCommandContext, ExtensionCommandHandler, ExtensionContext,
    ExtensionError, ExtensionHandler, ExtensionModule, ExtensionRegistrar,
    InputPoint, MessageEndPoint, MessageStartPoint, MessageUpdatePoint,
    ResourcesDiscoverPoint, StaticExtensionFactory, TurnEndPoint,
    TurnStartPoint,
};
use futures::{Stream, stream};
use kernel::{
    EventSink, KernelFactory, Model, ModelError, ModelFactory,
    NanoidIdGenerator,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentMessage, BeforeAgentStartEvent, BeforeAgentStartResult,
    ExtensionCommandDefinition, ExtensionDescriptor, ExtensionId,
    ExtensionUserMessage, InputEvent, InputResult, InputSource,
    MessageEndEvent, ModelFinal, ModelProfile, ModelRequest, ModelStreamEvent,
    ModelUsage, QueueKind, ResourcesDiscoverEvent, ResourcesDiscoverResult,
    RunRequest, SessionId, StaticExtensionRegistration, StopReason,
};
use skill::FilesystemSkillFactory;
use store::{JsonlStoreFactory, SessionCreateOptions, SystemClock};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

/// Model that emits one text update followed by a terminal response.
struct LifecycleModel {
    profile: ModelProfile,
}

#[async_trait]
impl Model for LifecycleModel {
    /// Returns the deterministic lifecycle-test profile.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Confirms that the fixture has no external readiness dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Streams one visible text fragment and a successful final result.
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
        Ok(Box::pin(stream::iter([
            Ok(ModelStreamEvent::TextDelta("ok".to_string())),
            Ok(ModelStreamEvent::Finished(ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: None,
                usage: ModelUsage::builder()
                    .input_tokens(1)
                    .output_tokens(1)
                    .cache_read_tokens(0)
                    .cache_write_tokens(0)
                    .total_tokens(2)
                    .build(),
            })),
        ])))
    }
}

/// Supplies the single deterministic lifecycle model.
struct LifecycleModelFactory;

impl ModelFactory for LifecycleModelFactory {
    /// Creates a fresh fixture model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(LifecycleModel {
            profile: ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id("lifecycle".to_string())
                .display_name("Lifecycle".to_string())
                .context_tokens(128_000)
                .max_output_tokens(8_000)
                .build(),
        }))
    }
}

/// Event sink that accepts protocol events while hooks record their own order.
struct DiscardSink;

#[async_trait]
impl EventSink for DiscardSink {
    /// Discards one live event.
    async fn emit(&self, _event: AgentEvent) -> Result<(), kernel::SinkError> {
        Ok(())
    }
}

/// Shared ordered hook recorder.
#[derive(Clone)]
struct LifecycleRecorder(Arc<Mutex<Vec<&'static str>>>);

macro_rules! observe_lifecycle {
    ($point:ty, $event:ty, $name:literal) => {
        #[async_trait]
        impl ExtensionHandler<$point> for LifecycleRecorder {
            /// Records one lifecycle observer invocation.
            async fn handle(
                &self,
                _event: &$event,
                _context: &ExtensionContext,
            ) -> Result<(), ExtensionError> {
                self.0.lock().expect("lifecycle lock").push($name);
                Ok(())
            }
        }
    };
}

observe_lifecycle!(AgentStartPoint, protocol::AgentStartEvent, "agent_start");
observe_lifecycle!(AgentEndPoint, protocol::AgentEndEvent, "agent_end");
observe_lifecycle!(
    AgentSettledPoint,
    protocol::AgentSettledEvent,
    "agent_settled"
);
observe_lifecycle!(TurnStartPoint, protocol::TurnStartEvent, "turn_start");
observe_lifecycle!(TurnEndPoint, protocol::TurnEndEvent, "turn_end");
observe_lifecycle!(
    MessageStartPoint,
    protocol::MessageStartEvent,
    "message_start"
);
observe_lifecycle!(
    MessageUpdatePoint,
    protocol::MessageUpdateEvent,
    "message_update"
);

#[async_trait]
impl ExtensionHandler<InputPoint> for LifecycleRecorder {
    /// Records input before allowing normal prompt expansion.
    async fn handle(
        &self,
        event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        let name = match event.source {
            InputSource::Extension => "input_extension",
            InputSource::Interactive | InputSource::Rpc => "input_rpc",
        };
        self.0.lock().expect("lifecycle lock").push(name);
        Ok(InputResult::Continue)
    }
}

/// Command that sends one idle extension-authored user message.
struct SendUserCommand;

#[async_trait]
impl ExtensionCommandHandler for SendUserCommand {
    /// Sends input through the normal extension source pipeline.
    async fn handle(
        &self,
        _arguments: &str,
        _parameters: &serde_json::Value,
        context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        context
            .event
            .send_user_message(ExtensionUserMessage {
                text: "from extension".to_string(),
                delivery: QueueKind::FollowUp,
                parent_entry_id: None,
            })
            .await
            .map_err(|error| ExtensionError::Handler(error.to_string()))
    }
}

/// Records slash-command arguments without entering the Agent lifecycle.
struct PromptOrderCommand(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl ExtensionCommandHandler for PromptOrderCommand {
    /// Records the textual argument tail supplied by slash dispatch.
    async fn handle(
        &self,
        arguments: &str,
        _parameters: &serde_json::Value,
        _context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionError> {
        self.0
            .lock()
            .expect("prompt order lock")
            .push(format!("command:{arguments}"));
        Ok(())
    }
}

/// Transforms sentinel inputs and records the prompt observed before Agent start.
#[derive(Clone)]
struct PromptOrderModule(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl ExtensionHandler<InputPoint> for PromptOrderModule {
    /// Records raw input and transforms only the Skill and Template sentinels.
    async fn handle(
        &self,
        event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        self.0
            .lock()
            .expect("prompt order lock")
            .push(format!("input:{}", event.text));
        Ok(match event.text.as_str() {
            "/raw-skill" => InputResult::Transform {
                text: "/skill:order argument".to_string(),
            },
            "/raw-template" => InputResult::Transform {
                text: "/template value".to_string(),
            },
            _ => InputResult::Continue,
        })
    }
}

#[async_trait]
impl ExtensionHandler<BeforeAgentStartPoint> for PromptOrderModule {
    /// Records the fully expanded prompt presented to the pre-Agent hook.
    async fn handle(
        &self,
        event: &BeforeAgentStartEvent,
        _context: &ExtensionContext,
    ) -> Result<BeforeAgentStartResult, ExtensionError> {
        let mut events = self.0.lock().expect("prompt order lock");
        events.push(format!(
            "options:{}:{}",
            event.system_prompt_options.cwd.display(),
            event.system_prompt_options.selected_tools.join(",")
        ));
        events.push(format!("before:{}", event.prompt));
        Ok(BeforeAgentStartResult::default())
    }
}

impl ExtensionModule for PromptOrderModule {
    /// Declares the input-order fixture identity.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("prompt-order").expect("extension id"),
            name: "Prompt order".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers slash dispatch, raw input transformation, and pre-Agent observation.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.register_command(
            ExtensionCommandDefinition {
                name: "handled".to_string(),
                description: None,
                argument_hint: None,
            },
            PromptOrderCommand(Arc::clone(&self.0)),
        )?;
        registrar.on::<InputPoint, _>(self.clone())?;
        registrar.on::<BeforeAgentStartPoint, _>(self.clone())
    }
}

#[async_trait]
impl ExtensionHandler<BeforeAgentStartPoint> for LifecycleRecorder {
    /// Records the pre-agent transformation point without changing the prompt.
    async fn handle(
        &self,
        _event: &BeforeAgentStartEvent,
        _context: &ExtensionContext,
    ) -> Result<BeforeAgentStartResult, ExtensionError> {
        self.0
            .lock()
            .expect("lifecycle lock")
            .push("before_agent_start");
        Ok(BeforeAgentStartResult::default())
    }
}

#[async_trait]
impl ExtensionHandler<MessageEndPoint> for LifecycleRecorder {
    /// Records message completion without replacing the message.
    async fn handle(
        &self,
        _event: &MessageEndEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<AgentMessage>, ExtensionError> {
        self.0.lock().expect("lifecycle lock").push("message_end");
        Ok(None)
    }
}

impl ExtensionModule for LifecycleRecorder {
    /// Returns the recorder extension descriptor.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("lifecycle").expect("extension id"),
            name: "Lifecycle".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers every run and message point asserted by this test.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<InputPoint, _>(self.clone())?;
        registrar.on::<BeforeAgentStartPoint, _>(self.clone())?;
        registrar.on::<AgentStartPoint, _>(self.clone())?;
        registrar.on::<AgentEndPoint, _>(self.clone())?;
        registrar.on::<AgentSettledPoint, _>(self.clone())?;
        registrar.on::<TurnStartPoint, _>(self.clone())?;
        registrar.on::<TurnEndPoint, _>(self.clone())?;
        registrar.on::<MessageStartPoint, _>(self.clone())?;
        registrar.on::<MessageUpdatePoint, _>(self.clone())?;
        registrar.on::<MessageEndPoint, _>(self.clone())?;
        registrar.register_command(
            ExtensionCommandDefinition {
                name: "send".to_string(),
                description: None,
                argument_hint: None,
            },
            SendUserCommand,
        )
    }
}

/// Agent and message hooks follow Pi's expansion, start, update, and end order.
#[tokio::test]
async fn run_dispatches_pi_lifecycle_order() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let order = Arc::new(Mutex::new(Vec::new()));
    let module_order = Arc::clone(&order);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::new(SystemClock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(LifecycleRecorder(Arc::clone(&module_order)))
                    as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-lifecycle").expect("session id");
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
                session_id,
                input: "hello".to_string(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("run agent");

    assert_eq!(
        *order.lock().expect("lifecycle lock"),
        vec![
            "input_rpc",
            "before_agent_start",
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_update",
            "message_end",
            "turn_end",
            "agent_end",
            "agent_settled",
        ]
    );
}

/// Idle extension user messages start a normal run with `InputSource::Extension`.
#[tokio::test]
async fn idle_extension_user_message_uses_the_normal_input_pipeline() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let order = Arc::new(Mutex::new(Vec::new()));
    let module_order = Arc::clone(&order);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::new(SystemClock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(LifecycleRecorder(Arc::clone(&module_order)))
                    as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-extension-input").expect("session id");
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
            "lifecycle/send".to_string(),
            serde_json::Value::Null,
        )
        .await
        .expect("send extension user message");

    let order = order.lock().expect("lifecycle lock");
    assert_eq!(order.first(), Some(&"input_extension"));
    assert!(order.contains(&"agent_settled"));
    let tree = kernel.session_tree(&session_id).expect("session tree");
    assert!(tree.entries.iter().any(|entry| {
        entry.kind == "message"
            && entry.payload["content"]["type"] == "user"
            && entry.payload["content"]["blocks"][0]["text"] == "from extension"
    }));
}

/// Slash dispatch, input hooks, Skill expansion, and Template expansion follow pi order.
#[tokio::test]
async fn prompt_order_matches_pi_before_agent_lifecycle() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    let config_root = root.path().join("config");
    let skill = config_root.join("skills/order/SKILL.md");
    let template = config_root.join("prompts/template.md");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    std::fs::create_dir_all(skill.parent().expect("Skill parent"))
        .expect("create Skill parent");
    std::fs::create_dir_all(template.parent().expect("Template parent"))
        .expect("create Template parent");
    std::fs::write(
        &skill,
        "---\nname: order\ndescription: Order Skill\n---\nSKILL EXPANDED\n",
    )
    .expect("write Skill");
    std::fs::write(&template, "TEMPLATE EXPANDED: $ARGUMENTS")
        .expect("write Template");
    let events = Arc::new(Mutex::new(Vec::new()));
    let module_events = Arc::clone(&events);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::new(SystemClock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            config_root.clone(),
            protocol::PromptPolicy::default(),
        )))
        .skill_factory(Some(Arc::new(
            FilesystemSkillFactory::builder()
                .user_home(config_root.join("test-home"))
                .global_root(config_root)
                .build(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(PromptOrderModule(Arc::clone(&module_events)))
                    as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-prompt-order").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");

    let handled = kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "/handled alpha beta".to_string(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("dispatch Extension command");
    assert!(handled.messages.is_empty());
    assert!(handled.turns.is_empty());
    assert_eq!(
        *events.lock().expect("prompt order lock"),
        vec!["command:alpha beta"]
    );

    events.lock().expect("prompt order lock").clear();
    kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "/raw-skill".to_string(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("run transformed Skill");
    let skill_events = events.lock().expect("prompt order lock").clone();
    assert_eq!(skill_events[0], "input:/raw-skill");
    assert!(skill_events[1].starts_with("options:"));
    assert!(skill_events[1].contains("read"));
    assert!(skill_events[2].starts_with("before:<skill name=\"order\""));
    assert!(skill_events[2].contains("SKILL EXPANDED"));
    assert!(skill_events[2].ends_with("argument"));

    events.lock().expect("prompt order lock").clear();
    kernel
        .run(
            RunRequest {
                session_id: session_id.clone(),
                input: "/raw-template".to_string(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("run transformed Template");
    let template_events = events.lock().expect("prompt order lock").clone();
    assert_eq!(template_events[0], "input:/raw-template");
    assert!(template_events[1].starts_with("options:"));
    assert_eq!(template_events[2], "before:TEMPLATE EXPANDED: value");

    events.lock().expect("prompt order lock").clear();
    let unknown = kernel
        .run(
            RunRequest {
                session_id,
                input: "/unknown".to_string(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("run unknown slash command");
    let unknown_events = events.lock().expect("prompt order lock").clone();
    assert_eq!(unknown_events[0], "input:/unknown");
    assert!(unknown_events[1].starts_with("options:"));
    assert_eq!(unknown_events[2], "before:/unknown");
    assert!(matches!(
        &unknown.messages[0].content,
        protocol::MessageContent::User { blocks }
            if blocks[0].text() == Some("/unknown")
    ));
}

/// Startup module that contributes one prompt directory selected by the test.
struct ResourceModule(PathBuf);

#[async_trait]
impl ExtensionHandler<ResourcesDiscoverPoint> for ResourceModule {
    /// Contributes the configured server-side prompt directory.
    async fn handle(
        &self,
        _event: &ResourcesDiscoverEvent,
        _context: &ExtensionContext,
    ) -> Result<ResourcesDiscoverResult, ExtensionError> {
        Ok(ResourcesDiscoverResult {
            skill_paths: Vec::new(),
            prompt_paths: vec![self.0.clone()],
        })
    }
}

impl ExtensionModule for ResourceModule {
    /// Returns the startup fixture descriptor.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("resources").expect("extension id"),
            name: "Resources".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers resource discovery for one session runtime.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar
            .on::<ResourcesDiscoverPoint, _>(ResourceModule(self.0.clone()))
    }
}

/// Unreadable discovered templates remain non-fatal and surface as Session diagnostics.
#[tokio::test]
async fn unreadable_extension_resource_is_reported_without_aborting_startup() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    let prompts = root.path().join("prompts");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    std::fs::create_dir_all(&prompts).expect("create prompts");
    let prompt = prompts.join("broken.md");
    std::fs::write(&prompt, [0xff]).expect("write invalid UTF-8");
    let module_prompts = prompts.clone();
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(JsonlStoreFactory::new(
            root.path(),
            Arc::new(SystemClock),
        )))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            vec![Arc::new(move || {
                Ok(Arc::new(ResourceModule(module_prompts.clone()))
                    as Arc<dyn ExtensionModule>)
            })],
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id = SessionId::try_from("session-retry").expect("session id");

    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("diagnostic-only resource failure must not abort startup");

    let diagnostics = kernel
        .prompt_diagnostics(&session_id)
        .expect("prompt diagnostics");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .source
            .path
            .as_ref()
            .is_some_and(|path| path == &prompt)
            && diagnostic
                .message
                .contains("failed to read Prompt Template")
    }));
    assert_eq!(kernel.list_sessions(None).expect("list sessions").len(), 1);
}
