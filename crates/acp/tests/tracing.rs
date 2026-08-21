use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use acp::{AcpServerFactory, AcpTransportKind};
use agent_client_protocol::Client;
use agent_client_protocol::schema::{ProtocolVersion, v2 as wire};
use extension::StaticExtensionFactory;
use futures::stream;
use kernel::{
    Kernel, KernelFactory, Model, ModelError, ModelFactory, ModelStream,
    NanoidIdGenerator,
};
use prompt::FilesystemPromptFactory;
use protocol::{
    IdGenerator, IdKind, ModelFinal, ModelProfile, ModelRequest,
    ModelStreamEvent, ModelUsage, StopReason,
};
use store::{JsonlStoreFactory, SystemClock};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::BuiltinToolFactory;

/// Shared in-memory writer used to inspect inherited span context.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    /// Returns all UTF-8 tracing output written by the subscriber.
    fn content(&self) -> String {
        String::from_utf8(self.0.lock().expect("log lock").clone())
            .expect("UTF-8 logs")
    }
}

/// One writer handle backed by the shared test buffer.
struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogWriter {
    /// Appends one formatted tracing buffer to the shared capture.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_poison_error| io::Error::other("log lock poisoned"))?
            .write(buffer)
    }

    /// The in-memory capture has no buffered state to flush.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
    type Writer = CapturedLogWriter;

    /// Creates a writer handle sharing the captured byte buffer.
    fn make_writer(&'writer self) -> Self::Writer {
        CapturedLogWriter(Arc::clone(&self.0))
    }
}

/// Produces deterministic operation identifiers independently from Kernel IDs.
#[derive(Default)]
struct TraceIds(AtomicUsize);

impl IdGenerator for TraceIds {
    /// Generates an incrementing identifier with the requested domain prefix.
    fn next(&self, kind: IdKind) -> String {
        let sequence = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        format!("{}-{}", kind.prefix(), sequence)
    }
}

/// Model that reports when the Provider boundary is reached by a Prompt task.
struct TracedModel {
    reached: Arc<Notify>,
    profile: ModelProfile,
}

#[async_trait::async_trait]
impl Model for TracedModel {
    /// Returns the deterministic profile used by the tracing integration test.
    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Confirms the local fixture is ready without external traffic.
    async fn preflight(&self) -> Result<(), ModelError> {
        Ok(())
    }

    /// Emits a diagnostic event under the inherited Prompt Trace.
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        tracing::info!("fixture provider received request");
        self.reached.notify_one();
        Ok(Box::pin(stream::iter([Ok(ModelStreamEvent::Finished(
            ModelFinal {
                stop_reason: StopReason::EndTurn,
                raw_stop_reason: Some("stop".to_string()),
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

/// Factory returning the one fixture model shared with the test observer.
struct TracedModelFactory {
    reached: Arc<Notify>,
}

impl ModelFactory for TracedModelFactory {
    /// Creates the deterministic model used by the ACP Prompt.
    fn create(&self) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::new(TracedModel {
            reached: Arc::clone(&self.reached),
            profile: ModelProfile::builder()
                .provider_id("fixture".to_string())
                .model_id("trace".to_string())
                .display_name("Trace fixture".to_string())
                .context_tokens(8_192)
                .max_output_tokens(1_024)
                .build(),
        }))
    }
}

/// Builds a production Kernel around the local deterministic model.
fn traced_kernel(root: &Path, reached: Arc<Notify>) -> Arc<Kernel> {
    let clock: Arc<dyn store::Clock> = Arc::new(SystemClock);
    Arc::new(
        KernelFactory::builder()
            .model_factory(Arc::new(TracedModelFactory { reached }))
            .tool_factory(Arc::new(BuiltinToolFactory::new()))
            .store_factory(Arc::new(JsonlStoreFactory::new(
                root,
                Arc::clone(&clock),
            )))
            .prompt_factory(Arc::new(FilesystemPromptFactory::new(
                root.join("config"),
                protocol::PromptPolicy::default(),
            )))
            .extension_factory(Arc::new(StaticExtensionFactory::default()))
            .clock(clock)
            .id_generator(Arc::new(NanoidIdGenerator))
            .build()
            .build()
            .expect("traced kernel"),
    )
}

/// Prompt background work keeps the ACP operation Trace through the Provider boundary.
#[tokio::test]
async fn prompt_trace_reaches_the_provider_background_task() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "info,acp::trace=debug",
        ))
        .with_writer(logs.clone())
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    let state = tempfile::tempdir().expect("store directory");
    let workspace = tempfile::tempdir().expect("workspace directory");
    let reached = Arc::new(Notify::new());
    let kernel = traced_kernel(state.path(), Arc::clone(&reached));
    let server = AcpServerFactory::new(
        kernel,
        Arc::new(TraceIds::default()),
        NonZeroUsize::new(128).expect("positive batch size"),
    )
    .component(AcpTransportKind::Stdio);

    Client
        .v2()
        .connect_with(server, async move |connection| {
            let diagnostic_meta = wire::Meta::from_iter([(
                "logging".to_string(),
                serde_json::json!({
                    "safeField": "visible-diagnostic",
                    "apiKey": "api-key-secret",
                    "nested": [{
                        "access_token": "access-token-secret",
                        "Authorization": "authorization-secret",
                        "cookie": "cookie-secret",
                        "dbPassword": "password-secret",
                        "clientSecret": "client-secret",
                        "input_tokens": 42,
                    }],
                }),
            )]);
            connection
                .send_request(
                    wire::InitializeRequest::new(
                        ProtocolVersion::V2,
                        wire::Implementation::new("trace-test", "0.1.0"),
                    )
                    .meta(diagnostic_meta),
                )
                .block_task()
                .await?;
            let session = connection
                .send_request(wire::NewSessionRequest::new(
                    workspace.path().to_path_buf(),
                ))
                .block_task()
                .await?;
            let session_id = session.session_id;
            connection
                .send_request(wire::PromptRequest::new(
                    session_id.clone(),
                    vec![
                        wire::ContentBlock::Text(wire::TextContent::new(
                            "sensitive prompt body",
                        )),
                        wire::ContentBlock::Image(wire::ImageContent::new(
                            "c2VjcmV0LWltYWdlLWJ5dGVz",
                            "image/png",
                        )),
                    ],
                ))
                .block_task()
                .await?;
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                reached.notified(),
            )
            .await
            .expect("provider boundary");
            connection.send_notification(
                wire::CancelSessionNotification::new(session_id),
            )?;
            connection
                .send_request(wire::ListSessionsRequest::new())
                .block_task()
                .await?;
            Ok(())
        })
        .await
        .expect("ACP trace connection");

    let output = logs.content();
    let provider_line = output
        .lines()
        .find(|line| line.contains("fixture provider received request"))
        .expect("provider log line")
        .to_string();
    assert!(provider_line.contains("trace_id=trace-3"));
    for lifecycle in ["started Kernel Run", "started Turn"] {
        let line = output
            .lines()
            .find(|line| line.contains(lifecycle))
            .unwrap_or_else(|| panic!("missing {lifecycle} log line"));
        assert!(line.contains("trace_id=trace-3"));
    }
    for lifecycle in [
        "started ACP request session/prompt:",
        "completed ACP request session/prompt:",
    ] {
        let line = output
            .lines()
            .find(|line| line.contains(lifecycle))
            .unwrap_or_else(|| panic!("missing {lifecycle} log line"));
        assert_eq!(line.matches("trace-3").count(), 1);
    }
    for operation in [
        "parameters for ACP request initialize:",
        "parameters for ACP request session/new:",
        "parameters for ACP request session/prompt:",
        "parameters for ACP notification session/cancel",
        "parameters for ACP request session/list:",
    ] {
        let line = output
            .lines()
            .find(|line| line.contains(operation))
            .unwrap_or_else(|| panic!("missing {operation} log line"));
        assert!(line.contains("trace_id=trace-"));
    }
    assert!(output.contains("sensitive prompt body"));
    assert!(output.contains("image data omitted: 18 bytes"));
    assert!(!output.contains("c2VjcmV0LWltYWdlLWJ5dGVz"));
    assert!(output.contains("visible-diagnostic"));
    assert!(output.contains("\"input_tokens\":42"));
    assert!(output.matches("[REDACTED]").count() >= 6);
    for secret in [
        "api-key-secret",
        "access-token-secret",
        "authorization-secret",
        "cookie-secret",
        "password-secret",
        "client-secret",
    ] {
        assert!(!output.contains(secret), "secret leaked: {secret}");
    }
}
