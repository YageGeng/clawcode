use std::io::{self, Write};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use extension::{
    ExtensionContext, ExtensionError, ExtensionHandler, ExtensionModule,
    ExtensionRegistrar, InputPoint, StaticExtensionFactory,
};
use futures::{Stream, stream};
use kernel::{
    EventSink, KernelFactory, Model, ModelError, ModelFactory, SinkError,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    AgentEvent, AgentEventPayload, EntryId, ExtensionDescriptor, ExtensionId,
    InputEvent, InputResult, LaneId, ModelFinal, ModelProfile, ModelRequest,
    ModelStreamEvent, ModelUsage, RunRequest, SessionId,
    StaticExtensionRegistration, StopReason,
};
use store::{
    EntryKind, JsonlStoreFactory, NewEntry, NewRecord, SessionCreateOptions,
    SessionEntry, SessionForkOptions, SessionMetadata, SessionRecord,
    SessionStore, StoreError, StoreFactory, SystemClock,
};
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

struct DiagnosticModel {
    profile: ModelProfile,
}

#[async_trait]
impl Model for DiagnosticModel {
    /// Returns the deterministic model profile used by diagnostic tests.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Confirms that the diagnostic fixture has no provider dependencies.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Completes the run after the extension failure has been reported.
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

struct DiagnosticModelFactory;

impl ModelFactory for DiagnosticModelFactory {
    /// Creates a deterministic model for diagnostic infrastructure tests.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(DiagnosticModel {
            profile: ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id("diagnostic".to_string())
                .display_name("Diagnostic fixture".to_string())
                .context_tokens(128_000)
                .max_output_tokens(1_024)
                .build(),
        }))
    }
}

struct FailingInput;

#[async_trait]
impl ExtensionHandler<InputPoint> for FailingInput {
    /// Produces the extension failure whose reporting infrastructure is exercised.
    async fn handle(
        &self,
        _event: &InputEvent,
        _context: &ExtensionContext,
    ) -> Result<InputResult, ExtensionError> {
        Err(ExtensionError::Handler(
            "diagnostic fixture failure".to_string(),
        ))
    }
}

struct FailingModule;

impl ExtensionModule for FailingModule {
    /// Declares the stable extension identity expected in diagnostic logs.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from("diagnostic-fixture")
                .expect("extension id"),
            name: "Diagnostic fixture".to_string(),
            version: "1".to_string(),
        }
    }

    /// Registers the failing input handler without changing agent control flow.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<InputPoint, _>(FailingInput)
    }
}

struct FailingDiagnosticStoreFactory {
    inner: JsonlStoreFactory,
}

impl StoreFactory for FailingDiagnosticStoreFactory {
    /// Creates a normal session wrapped with selective diagnostic persistence failure.
    fn create(
        &self,
        options: SessionCreateOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(FailingDiagnosticStore {
            inner: self.inner.create(options)?,
        }))
    }

    /// Opens a normal session wrapped with selective diagnostic persistence failure.
    fn open(&self, path: &Path) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(FailingDiagnosticStore {
            inner: self.inner.open(path)?,
        }))
    }

    /// Forks a normal session wrapped with selective diagnostic persistence failure.
    fn fork(
        &self,
        source_path: &Path,
        options: SessionForkOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(FailingDiagnosticStore {
            inner: self.inner.fork(source_path, options)?,
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

struct FailingDiagnosticStore {
    inner: Box<dyn SessionStore>,
}

impl SessionStore for FailingDiagnosticStore {
    /// Returns the delegated session identifier.
    fn session_id(&self) -> &SessionId {
        self.inner.session_id()
    }

    /// Returns the delegated session file path.
    fn path(&self) -> &Path {
        self.inner.path()
    }

    /// Delegates lane creation to the real store.
    fn create_lane(
        &mut self,
        lane: LaneId,
        at: Option<EntryId>,
    ) -> Result<(), StoreError> {
        self.inner.create_lane(lane, at)
    }

    /// Delegates lane movement to the real store.
    fn move_lane(
        &mut self,
        lane: &LaneId,
        to: Option<EntryId>,
    ) -> Result<(), StoreError> {
        self.inner.move_lane(lane, to)
    }

    /// Rejects only extension handler diagnostic messages and persists all other entries.
    fn append_entry(
        &mut self,
        lane: &LaneId,
        entry: NewEntry,
    ) -> Result<SessionEntry, StoreError> {
        let is_diagnostic = entry.kind == EntryKind::Message
            && entry.payload.pointer("/content/extension/details/kind")
                == Some(&serde_json::json!("handler_error"));
        if is_diagnostic {
            return Err(StoreError::InvalidSession(
                "forced diagnostic append failure".to_string(),
            ));
        }
        self.inner.append_entry(lane, entry)
    }

    /// Returns the delegated lane leaf.
    fn lane(&self, lane: &LaneId) -> Option<&EntryId> {
        self.inner.lane(lane)
    }

    /// Returns the delegated stored entry.
    fn get_entry(&self, id: &EntryId) -> Option<&SessionEntry> {
        self.inner.get_entry(id)
    }

    /// Returns all delegated entries.
    fn entries(&self) -> Vec<SessionEntry> {
        self.inner.entries()
    }

    /// Returns the delegated branch.
    fn branch(&self, leaf: &EntryId) -> Result<Vec<SessionEntry>, StoreError> {
        self.inner.branch(leaf)
    }

    /// Delegates record appends to the real store.
    fn append_record(
        &mut self,
        record: NewRecord,
    ) -> Result<SessionRecord, StoreError> {
        self.inner.append_record(record)
    }

    /// Returns all delegated records.
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

struct SelectiveFailingSink;

#[async_trait]
impl EventSink for SelectiveFailingSink {
    /// Rejects only diagnostic events so the surrounding agent run can continue.
    async fn emit(&self, event: AgentEvent) -> Result<(), SinkError> {
        if matches!(
            event.payload,
            AgentEventPayload::ExtensionHandlerFailed { .. }
        ) {
            return Err(SinkError::Consumer(
                "forced diagnostic sink failure".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    /// Returns the complete UTF-8 tracing output captured by this writer.
    fn content(&self) -> String {
        String::from_utf8(self.0.lock().expect("log lock").clone())
            .expect("UTF-8 logs")
    }
}

struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogWriter {
    /// Appends one formatted tracing buffer to the shared test capture.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_poison_error| io::Error::other("log lock poisoned"))?
            .write(buffer)
    }

    /// Tracing uses an in-memory writer, so flushing has no additional work.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
    type Writer = CapturedLogWriter;

    /// Creates a writer sharing this test's in-memory byte buffer.
    fn make_writer(&'writer self) -> Self::Writer {
        CapturedLogWriter(Arc::clone(&self.0))
    }
}

/// Diagnostic persistence and sink failures are logged without aborting the agent run.
#[tokio::test]
async fn diagnostic_infrastructure_failures_are_logged() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(logs.clone())
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    let root = tempfile::tempdir().expect("store root");
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    let kernel = KernelFactory::builder()
        .model_factory(Arc::new(DiagnosticModelFactory))
        .tool_factory(Arc::new(BuiltinToolFactory::new()))
        .store_factory(Arc::new(FailingDiagnosticStoreFactory {
            inner: JsonlStoreFactory::new(root.path(), Arc::clone(&clock)),
        }))
        .prompt_factory(Arc::new(FilesystemPromptFactory::new(
            root.path().join("config"),
            protocol::PromptPolicy::default(),
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
        SessionId::try_from("diagnostic-observability").expect("session id");
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
                input: "prompt-secret-must-not-be-logged".to_string(),
            },
            Arc::new(SelectiveFailingSink),
        )
        .await
        .expect("diagnostic failures must not abort the run");

    let output = logs.content();
    assert!(output.contains("for extension diagnostic-fixture at input"));
    assert!(output.contains("during persist"));
    assert!(output.contains("during sink"));
    assert!(!output.contains("prompt-secret-must-not-be-logged"));
}
