use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use extension::{Extension, StaticExtensionFactory};
use futures::{Stream, stream};
use kernel::{
    EventSink, Kernel, KernelFactory, Model, ModelError, ModelFactory,
};
use protocol::{
    AgentEvent, AgentEventPayload, ExtensionContext, ExtensionDirective,
    ExtensionEvent, IdGenerator, IdKind, McpConnectionState, McpServerInfo,
    ModelFailure, ModelFinal, ModelProfile, ModelRequest,
    ModelRetryDisposition, ModelStreamEvent, ModelUsage, QueueKind, RunRequest,
    SessionId, SessionTitle, StopReason, TimestampMs,
};
use skill::FilesystemSkillFactory;
use store::{Clock, JsonlStoreFactory, SessionCreateOptions};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::{BuiltinToolFactory, ToolRegistry};

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
    /// Produces one connected server and an empty registry for status tests.
    async fn create(&self) -> Result<mcp::McpSession, mcp::McpError> {
        Ok(mcp::McpSession {
            tools: ToolRegistry::default(),
            servers: vec![
                McpServerInfo::builder()
                    .name("docs".to_string())
                    .state(McpConnectionState::Connected)
                    .build(),
            ],
        })
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

struct RecordingExtension(Arc<Mutex<Vec<ExtensionEvent>>>);

#[async_trait]
impl Extension for RecordingExtension {
    /// Records session lifecycle hooks while allowing each operation to continue.
    async fn handle(
        &self,
        event: &ExtensionEvent,
        _context: &ExtensionContext,
    ) -> Result<ExtensionDirective, extension::ExtensionError> {
        self.0.lock().expect("extension lock").push(event.clone());
        Ok(ExtensionDirective::Continue)
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
            root,
            Arc::clone(&clock),
        )))
        .extension_factory(Arc::new(StaticExtensionFactory::new(Vec::new())))
        .clock(clock)
        .id_generator(ids);
    let factory = match skill_root {
        Some(skill_root) => builder
            .skill_factory(Some(Arc::new(FilesystemSkillFactory::new(vec![
                skill_root,
            ]))))
            .build(),
        None => builder.build(),
    };
    Arc::new(factory.build().expect("build kernel"))
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
        .extension_factory(Arc::new(StaticExtensionFactory::new(vec![
            Arc::new(RecordingExtension(Arc::clone(&extension_events))),
        ])))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
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
    for expected in [
        ExtensionEvent::ModelSelect,
        ExtensionEvent::ThinkingLevelSelect,
        ExtensionEvent::SessionInfoChanged,
        ExtensionEvent::SessionBeforeSwitch,
        ExtensionEvent::SessionSwitch,
    ] {
        assert!(events.contains(&expected), "missing {expected}");
    }
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
                    input: "hold".to_string(),
                },
                Arc::new(RecordingSink::default()),
            )
            .await
    });
    entered.notified().await;

    let queued = kernel
        .queue_message(&session_id, QueueKind::FollowUp, "next".to_string())
        .expect("queue");
    assert!(!queued.message.identity.turn_id.as_str().is_empty());
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
                input: "  First line\nsecond line".to_string(),
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
    let skill_path = skill_root.path().join("SKILL.md");
    fs::write(
        &skill_path,
        "---\nname: review\ndescription: Review code\n---\nSECRET BODY\n",
    )
    .expect("write skill");
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

    let skills = kernel.skills();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "review");
    assert_eq!(skills[0].description, "Review code");
    assert_eq!(skills[0].path, skill_path);
    assert!(
        !serde_json::to_string(&skills)
            .expect("serialize")
            .contains("SECRET")
    );
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
        .extension_factory(Arc::new(StaticExtensionFactory::new(Vec::new())))
        .clock(clock)
        .id_generator(Arc::new(SequentialIds(AtomicU64::new(0))))
        .mcp_factory(Some(Arc::new(StaticMcpFactory)))
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
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].name, "docs");
    assert_eq!(status[0].state, McpConnectionState::Connected);
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
