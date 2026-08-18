use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
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
    AgentEvent, AgentEventPayload, AgentMessage, BeforeAgentStartEvent,
    BeforeAgentStartResult, CompactionPolicy, EntryId,
    ExtensionCommandDefinition, ExtensionDescriptor, ExtensionId,
    ExtensionUserMessage, InputEvent, InputResult, InputSource, LaneId,
    MessageContent, MessageEndEvent, ModelFinal, ModelProfile, ModelRequest,
    ModelStreamEvent, ModelUsage, QueueKind, ResourcesDiscoverEvent,
    ResourcesDiscoverResult, RunRequest, SessionId, SessionTitle,
    StaticExtensionRegistration, StopReason, UserBashRequest,
};
use skill::FilesystemSkillFactory;
use store::{
    EntryKind, JsonlStoreFactory, NewEntry, NewRecord, RecordKind,
    SessionCreateOptions, SessionEntry, SessionForkOptions, SessionMetadata,
    SessionRecord, SessionStore, StoreError, StoreFactory, SystemClock,
};
use tokio::sync::Notify;
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

/// Store factory that records mutation and durability boundaries around real JSONL stores.
struct DurabilityStoreFactory {
    inner: JsonlStoreFactory,
    order: Arc<Mutex<Vec<&'static str>>>,
    fail_sync: Arc<AtomicBool>,
}

impl StoreFactory for DurabilityStoreFactory {
    /// Creates one instrumented real store.
    fn create(
        &self,
        options: SessionCreateOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(DurabilityStore {
            inner: self.inner.create(options)?,
            order: Arc::clone(&self.order),
            fail_sync: Arc::clone(&self.fail_sync),
        }))
    }

    /// Opens one instrumented real store.
    fn open(&self, path: &Path) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(DurabilityStore {
            inner: self.inner.open(path)?,
            order: Arc::clone(&self.order),
            fail_sync: Arc::clone(&self.fail_sync),
        }))
    }

    /// Forks one instrumented real store.
    fn fork(
        &self,
        source_path: &Path,
        options: SessionForkOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        let inner = self.inner.fork(source_path, options)?;
        self.order.lock().expect("durability lock").push("forked");
        Ok(Box::new(DurabilityStore {
            inner,
            order: Arc::clone(&self.order),
            fail_sync: Arc::clone(&self.fail_sync),
        }))
    }

    /// Delegates session discovery.
    fn list(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<SessionMetadata>, StoreError> {
        self.inner.list(cwd)
    }

    /// Delegates session deletion.
    fn delete(&self, session_id: &SessionId) -> Result<(), StoreError> {
        self.inner.delete(session_id)
    }
}

/// Real session store that exposes selected mutation and sync order to tests.
struct DurabilityStore {
    inner: Box<dyn SessionStore>,
    order: Arc<Mutex<Vec<&'static str>>>,
    fail_sync: Arc<AtomicBool>,
}

impl SessionStore for DurabilityStore {
    /// Returns the delegated session identifier.
    fn session_id(&self) -> &SessionId {
        self.inner.session_id()
    }

    /// Returns the delegated session path.
    fn path(&self) -> &Path {
        self.inner.path()
    }

    /// Delegates lane creation.
    fn create_lane(
        &mut self,
        lane: LaneId,
        at: Option<EntryId>,
    ) -> Result<(), StoreError> {
        self.inner.create_lane(lane, at)
    }

    /// Delegates lane movement.
    fn move_lane(
        &mut self,
        lane: &LaneId,
        to: Option<EntryId>,
    ) -> Result<(), StoreError> {
        self.order
            .lock()
            .expect("durability lock")
            .push("move_lane");
        self.inner.move_lane(lane, to)
    }

    /// Delegates entry persistence.
    fn append_entry(
        &mut self,
        lane: &LaneId,
        entry: NewEntry,
    ) -> Result<SessionEntry, StoreError> {
        if entry.kind == EntryKind::Message
            && entry.payload.pointer("/content/type")
                == Some(&serde_json::json!("bash_execution"))
        {
            self.order
                .lock()
                .expect("durability lock")
                .push("bash_message");
        }
        self.inner.append_entry(lane, entry)
    }

    /// Returns the delegated lane leaf.
    fn lane(&self, lane: &LaneId) -> Option<&EntryId> {
        self.inner.lane(lane)
    }

    /// Returns one delegated entry.
    fn get_entry(&self, id: &EntryId) -> Option<&SessionEntry> {
        self.inner.get_entry(id)
    }

    /// Returns all delegated entries.
    fn entries(&self) -> Vec<SessionEntry> {
        self.inner.entries()
    }

    /// Returns one delegated branch.
    fn branch(&self, leaf: &EntryId) -> Result<Vec<SessionEntry>, StoreError> {
        self.inner.branch(leaf)
    }

    /// Records selected operation mutations before delegating persistence.
    fn append_record(
        &mut self,
        record: NewRecord,
    ) -> Result<SessionRecord, StoreError> {
        let marker = match record.kind {
            RecordKind::StepAttempt => Some("step_attempt"),
            RecordKind::OperationFinished => Some("operation_finished"),
            RecordKind::QueueEnqueued => Some("queue_enqueued"),
            RecordKind::QueueCancelled => Some("queue_cancelled"),
            RecordKind::OperationStarted
            | RecordKind::AbortRequested
            | RecordKind::ToolStarted
            | RecordKind::WriteDeferred
            | RecordKind::Usage => None,
        };
        if let Some(marker) = marker {
            self.order.lock().expect("durability lock").push(marker);
        }
        self.inner.append_record(record)
    }

    /// Records and delegates one durability checkpoint.
    fn sync(&mut self) -> Result<(), StoreError> {
        self.order.lock().expect("durability lock").push("sync");
        if self.fail_sync.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("injected sync failure").into());
        }
        self.inner.sync()
    }

    /// Returns all delegated records.
    fn records(&self) -> Vec<SessionRecord> {
        self.inner.records()
    }

    /// Records and delegates session-name persistence.
    fn set_name(&mut self, name: Option<String>) -> Result<(), StoreError> {
        self.order.lock().expect("durability lock").push("set_name");
        self.inner.set_name(name)
    }

    /// Returns the delegated session name.
    fn name(&self) -> Option<&str> {
        self.inner.name()
    }

    /// Delegates entry-label persistence.
    fn set_label(
        &mut self,
        target_id: EntryId,
        label: Option<String>,
    ) -> Result<(), StoreError> {
        self.inner.set_label(target_id, label)
    }

    /// Returns the delegated entry label.
    fn label(&self, target_id: &EntryId) -> Option<&str> {
        self.inner.label(target_id)
    }
}

/// Event sink that records externally visible terminal boundaries.
struct DurabilitySink(Arc<Mutex<Vec<&'static str>>>);

#[async_trait]
impl EventSink for DurabilitySink {
    /// Records only events whose publication must follow a durability checkpoint.
    async fn emit(&self, event: AgentEvent) -> Result<(), kernel::SinkError> {
        let marker = match event.payload {
            AgentEventPayload::TurnEnd { .. } => Some("turn_end"),
            AgentEventPayload::RunEnd { .. } => Some("run_end"),
            AgentEventPayload::AgentSettled { .. } => Some("agent_settled"),
            AgentEventPayload::CompactionEnd { .. } => Some("compaction_end"),
            AgentEventPayload::MessageEnd { message }
                if matches!(
                    message.content,
                    MessageContent::BashExecution { .. }
                ) =>
            {
                Some("bash_message_end")
            }
            _ => None,
        };
        if let Some(marker) = marker {
            self.0.lock().expect("durability lock").push(marker);
        }
        Ok(())
    }
}

/// Model that pauses one active run so queue operations can be observed in isolation.
struct BlockingModel {
    profile: ModelProfile,
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Model for BlockingModel {
    /// Returns the deterministic blocking-model profile.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Confirms that the fixture has no external readiness dependency.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Waits for test release before returning one successful terminal event.
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
        self.started.notify_one();
        self.release.notified().await;
        Ok(Box::pin(stream::iter([Ok(ModelStreamEvent::Finished(
            ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: None,
                usage: ModelUsage::builder()
                    .input_tokens(1)
                    .output_tokens(0)
                    .cache_read_tokens(0)
                    .cache_write_tokens(0)
                    .total_tokens(1)
                    .build(),
            },
        ))])))
    }
}

/// Supplies one shared blocking model to a durability test.
struct BlockingModelFactory(Arc<BlockingModel>);

impl ModelFactory for BlockingModelFactory {
    /// Returns the shared blocking model.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.0) as Arc<dyn Model>)
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
                input: "hello".into(),
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

/// Turn and Run terminal events are published only after their persisted state is synced.
#[tokio::test]
async fn terminal_events_follow_durability_checkpoints() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let order = Arc::new(Mutex::new(Vec::new()));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(DurabilityStoreFactory {
            inner: JsonlStoreFactory::new(root.path(), Arc::new(SystemClock)),
            order: Arc::clone(&order),
            fail_sync: Arc::new(AtomicBool::new(false)),
        }))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            Vec::new(),
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-durability-order").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    order.lock().expect("durability lock").clear();

    kernel
        .run(
            RunRequest {
                session_id,
                input: "hello".into(),
            },
            Arc::new(DurabilitySink(Arc::clone(&order))),
        )
        .await
        .expect("run agent");

    let order = order.lock().expect("durability lock");
    assert!(
        order
            .windows(3)
            .any(|window| { window == ["step_attempt", "sync", "turn_end"] })
    );
    assert!(
        order.windows(3).any(|window| {
            window == ["operation_finished", "sync", "run_end"]
        })
    );
    assert!(
        order
            .windows(3)
            .any(|window| window == ["run_end", "sync", "agent_settled"])
    );
}

/// A failed startup checkpoint unregisters and deletes the partial new session.
#[tokio::test]
async fn create_session_sync_failure_rolls_back_registration() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let order = Arc::new(Mutex::new(Vec::new()));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(DurabilityStoreFactory {
            inner: JsonlStoreFactory::new(root.path(), Arc::new(SystemClock)),
            order,
            fail_sync: Arc::new(AtomicBool::new(true)),
        }))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            Vec::new(),
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-sync-rollback").expect("session id");

    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect_err("startup sync should fail");

    assert!(matches!(
        kernel
            .close_session(&session_id)
            .await
            .expect_err("partial session should be unregistered"),
        kernel::KernelError::SessionNotFound(id) if id == session_id
    ));
    assert!(
        kernel
            .list_sessions(None)
            .expect("list persisted sessions")
            .is_empty()
    );
}

/// Queue acceptance and cancellation are synced before either API reports success.
#[tokio::test]
async fn queue_mutations_sync_before_returning() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let order = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(BlockingModel {
        profile: ModelProfile::builder()
            .provider_id("fixture".to_string())
            .model_id("blocking".to_string())
            .display_name("Blocking".to_string())
            .context_tokens(128_000)
            .max_output_tokens(8_000)
            .build(),
        started: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    });
    let kernel = Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(BlockingModelFactory(Arc::clone(&model))))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(DurabilityStoreFactory {
                inner: JsonlStoreFactory::new(
                    root.path(),
                    Arc::new(SystemClock),
                ),
                order: Arc::clone(&order),
                fail_sync: Arc::new(AtomicBool::new(false)),
            }))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.path().join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::new(
                StaticExtensionRegistration::default(),
                Vec::new(),
            )))
            .clock(Arc::new(SystemClock))
            .id_generator(Arc::new(NanoidIdGenerator))
            .build()
            .build()
            .expect("build kernel"),
    );
    let session_id =
        SessionId::try_from("session-queue-durability").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    order.lock().expect("durability lock").clear();

    let run_kernel = Arc::clone(&kernel);
    let run_session_id = session_id.clone();
    let run = tokio::spawn(async move {
        run_kernel
            .run(
                RunRequest {
                    session_id: run_session_id,
                    input: "hold".into(),
                },
                Arc::new(DiscardSink),
            )
            .await
    });
    model.started.notified().await;

    let queued = kernel
        .queue_message(&session_id, QueueKind::FollowUp, "queued".to_string())
        .await
        .expect("queue message");
    {
        let order = order.lock().expect("durability lock");
        assert!(
            order
                .windows(2)
                .any(|window| window == ["queue_enqueued", "sync"])
        );
    }

    order.lock().expect("durability lock").clear();
    kernel
        .remove_pending_message(&session_id, &queued.queue_id)
        .expect("remove queued message");
    {
        let order = order.lock().expect("durability lock");
        assert!(
            order
                .windows(2)
                .any(|window| window == ["queue_cancelled", "sync"])
        );
    }

    model.release.notify_one();
    run.await.expect("join run").expect("complete blocked run");
}

/// Session metadata changes are synced before their public operation completes.
#[tokio::test]
async fn rename_session_syncs_before_returning() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let order = Arc::new(Mutex::new(Vec::new()));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(DurabilityStoreFactory {
            inner: JsonlStoreFactory::new(root.path(), Arc::new(SystemClock)),
            order: Arc::clone(&order),
            fail_sync: Arc::new(AtomicBool::new(false)),
        }))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            Vec::new(),
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-rename-durability").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    order.lock().expect("durability lock").clear();

    kernel
        .rename_session(
            &session_id,
            SessionTitle::try_from("durable title").expect("session title"),
        )
        .await
        .expect("rename session");

    assert_eq!(
        *order.lock().expect("durability lock"),
        vec!["set_name", "sync"]
    );
}

/// Session navigation, forking, and shutdown each end with a durability checkpoint.
#[tokio::test]
async fn session_maintenance_operations_sync_before_returning() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let order = Arc::new(Mutex::new(Vec::new()));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(DurabilityStoreFactory {
            inner: JsonlStoreFactory::new(root.path(), Arc::new(SystemClock)),
            order: Arc::clone(&order),
            fail_sync: Arc::new(AtomicBool::new(false)),
        }))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            Vec::new(),
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id = SessionId::try_from("session-maintenance-durability")
        .expect("session id");
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
                input: "hello".into(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("prepare session branch");
    let leaf = kernel
        .session_tree(&session_id)
        .expect("session tree")
        .leaf_id
        .expect("session leaf");

    order.lock().expect("durability lock").clear();
    kernel
        .navigate_session(&session_id, None)
        .await
        .expect("navigate to root");
    assert_eq!(
        *order.lock().expect("durability lock"),
        vec!["move_lane", "sync"]
    );

    order.lock().expect("durability lock").clear();
    kernel
        .fork_session(
            &session_id,
            leaf,
            SessionId::try_from("session-maintenance-fork")
                .expect("fork session id"),
            cwd,
        )
        .await
        .expect("fork session");
    assert_eq!(
        *order.lock().expect("durability lock"),
        vec!["forked", "sync"]
    );

    order.lock().expect("durability lock").clear();
    kernel
        .close_session(&session_id)
        .await
        .expect("close session");
    assert_eq!(*order.lock().expect("durability lock"), vec!["sync"]);
}

/// User Bash output is durable before its complete message event is published.
#[tokio::test]
async fn user_bash_message_syncs_before_publication() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let order = Arc::new(Mutex::new(Vec::new()));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(DurabilityStoreFactory {
            inner: JsonlStoreFactory::new(root.path(), Arc::new(SystemClock)),
            order: Arc::clone(&order),
            fail_sync: Arc::new(AtomicBool::new(false)),
        }))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            Vec::new(),
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
        .build()
        .build()
        .expect("build kernel");
    let session_id =
        SessionId::try_from("session-bash-durability").expect("session id");
    kernel
        .create_session(SessionCreateOptions {
            session_id: session_id.clone(),
            cwd,
            parent_session_id: None,
        })
        .await
        .expect("create session");
    order.lock().expect("durability lock").clear();

    kernel
        .execute_user_bash(
            UserBashRequest {
                session_id,
                command: "printf durability".to_string(),
                exclude_from_context: false,
            },
            Arc::new(DurabilitySink(Arc::clone(&order))),
        )
        .await
        .expect("execute user bash");

    let order = order.lock().expect("durability lock");
    assert!(order.windows(3).any(|window| {
        window == ["bash_message", "sync", "bash_message_end"]
    }));
}

/// Manual compaction publishes its terminal event only after the recovery record is synced.
#[tokio::test]
async fn compaction_end_follows_its_durability_checkpoint() {
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let order = Arc::new(Mutex::new(Vec::new()));
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(LifecycleModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(DurabilityStoreFactory {
            inner: JsonlStoreFactory::new(root.path(), Arc::new(SystemClock)),
            order: Arc::clone(&order),
            fail_sync: Arc::new(AtomicBool::new(false)),
        }))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(
            StaticExtensionRegistration::default(),
            Vec::new(),
        )))
        .clock(Arc::new(SystemClock))
        .id_generator(Arc::new(NanoidIdGenerator))
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
    let session_id = SessionId::try_from("session-compaction-durability")
        .expect("session id");
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
                input: "history".into(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("prepare compaction history");
    order.lock().expect("durability lock").clear();

    kernel
        .compact_session(
            &session_id,
            Arc::new(DurabilitySink(Arc::clone(&order))),
        )
        .await
        .expect("compact session");

    let order = order.lock().expect("durability lock");
    assert!(order.windows(3).any(|window| {
        window == ["operation_finished", "sync", "compaction_end"]
    }));
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
            "lifecycle:send".to_string(),
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
                input: "/handled alpha beta".into(),
            },
            Arc::new(DiscardSink),
        )
        .await
        .expect("dispatch Extension command");
    assert!(matches!(
        &handled.messages[0].content,
        protocol::MessageContent::SlashCommand {
            message: protocol::SlashCommandMessage::Invocation {
                invocation,
                ..
            }
        } if invocation.original == "/handled alpha beta"
    ));
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
                input: "/raw-skill".into(),
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
                input: "/raw-template".into(),
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
                input: "/unknown".into(),
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
