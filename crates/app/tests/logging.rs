use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use app::LoggingFactory;
use protocol::ProductIdentity;

/// Shared in-memory writer used to inspect formatted production tracing output.
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

/// The logging factory honors its immutable filter and formats readable event text.
#[test]
fn logging_factory_filters_debug_and_writes_info_messages() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("info", false)
        .expect("valid filter")
        .build(logs.clone());

    tracing::subscriber::with_default(subscriber, || {
        tracing::debug!("hidden diagnostic");
        tracing::info!("started stdio transport");
    });

    let output = logs.content();
    assert!(output.contains("started stdio transport"));
    assert!(!output.contains("hidden diagnostic"));
    assert!(!output.contains("\u{1b}["));
}

/// Explicit color configuration emits ANSI escape sequences.
#[test]
fn logging_factory_emits_ansi_when_color_is_enabled() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("info", true)
        .expect("valid filter")
        .build(logs.clone());

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("colored diagnostic");
    });

    assert!(logs.content().contains("\u{1b}["));
}

/// Compact logs expose local millisecond time and the exact event source line.
#[test]
fn logging_factory_writes_compact_timestamp_and_source_location() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("info", false)
        .expect("valid filter")
        .build(logs.clone());

    let expected_line = line!() + 2;
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("source location diagnostic");
    });

    let output = logs.content();
    let (timestamp, remainder) = output
        .split_once(" INFO  [-] ")
        .expect("compact level and empty trace marker");
    assert_eq!(timestamp.len(), 23);
    assert_eq!(timestamp.as_bytes().get(4), Some(&b'-'));
    assert_eq!(timestamp.as_bytes().get(7), Some(&b'-'));
    assert_eq!(timestamp.as_bytes().get(10), Some(&b' '));
    assert_eq!(timestamp.as_bytes().get(13), Some(&b':'));
    assert_eq!(timestamp.as_bytes().get(16), Some(&b':'));
    assert_eq!(timestamp.as_bytes().get(19), Some(&b'.'));
    assert!(remainder.contains(&format!(
        "crates/app/tests/logging.rs:{expected_line} | source location diagnostic"
    )));
}

/// Nested tracing spans contribute one Trace ID without exposing their names.
#[test]
fn logging_factory_flattens_span_context_to_one_trace_id() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("info", false)
        .expect("valid filter")
        .build(logs.clone());

    tracing::subscriber::with_default(subscriber, || {
        let trace = tracing::info_span!(
            "acp_operation",
            trace_id = "trace-format-test"
        );
        trace.in_scope(|| {
            let provider = tracing::info_span!("provider_request");
            provider.in_scope(|| tracing::info!("provider completed"));
        });
    });

    let output = logs.content();
    assert_eq!(output.matches("trace-format-test").count(), 1);
    assert!(output.contains(" INFO  [trace-format-test] "));
    assert!(!output.contains("acp_operation"));
    assert!(!output.contains("provider_request"));
}

/// ACP handler diagnostics omit the recursively escaped handler chain.
#[test]
fn logging_factory_compacts_acp_handler_chain_diagnostics() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("warn", false)
        .expect("valid filter")
        .build(logs.clone());
    let escaped_handler = format!(
        "NamedHandler({:?}, {:?})",
        "agent_client_protocol_schema::v2::agent::InitializeRequest",
        "\\\"\\\\\\\"\\\\\\\\\\\\\\\"(null)"
    );

    tracing::subscriber::with_default(subscriber, || {
        // This reproduces the structured event emitted by the external ACP SDK.
        tracing::warn!(
            target: "agent_client_protocol::jsonrpc::incoming_actor",
            method = ?"_clawcode/mcp/status",
            id = ?Some(10_u64),
            err = ?"session not found: session-test",
            handler = ?escaped_handler,
            "Handler errored"
        );
    });

    let output = logs.content();
    assert!(output.contains("Handler errored"));
    assert!(output.contains("method=\"_clawcode/mcp/status\""));
    assert!(output.contains("id=Some(10)"));
    assert!(output.contains("session not found: session-test"));
    assert!(output.contains("handler=<omitted>"));
    assert!(!output.contains("NamedHandler"));
    assert!(output.len() < 512, "unexpectedly large log: {output}");
}

/// Process startup uses the TOML filter unless `RUST_LOG` explicitly overrides it.
#[test]
fn process_logging_prefers_environment_over_toml_filter() {
    let directory = tempfile::tempdir().expect("logging config directory");
    let config_path = directory.path().join("claw.toml");
    std::fs::write(
        &config_path,
        r#"
active_model = "fixture/model"

[logging]
filter = "off"

[[providers]]
id = "fixture"
display_name = "Fixture"
base_url = "https://example.invalid"
api_key = "sk-test"

[[providers.models]]
id = "model"
display_name = "Fixture Model"
context_tokens = 32768
max_output_tokens = 1024
"#,
    )
    .expect("write logging config");

    let configured = Command::new(env!("CARGO_BIN_EXE_clawcode"))
        .arg("stdio")
        .env(ProductIdentity::CONFIG_PATH_ENV, &config_path)
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run configured logging process");
    assert!(
        configured.status.success(),
        "configured process failed: {}",
        String::from_utf8_lossy(&configured.stderr)
    );
    assert!(configured.stdout.is_empty());
    assert!(configured.stderr.is_empty());

    let overridden = Command::new(env!("CARGO_BIN_EXE_clawcode"))
        .arg("stdio")
        .env(ProductIdentity::CONFIG_PATH_ENV, &config_path)
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .output()
        .expect("run environment logging process");
    assert!(
        overridden.status.success(),
        "overridden process failed: {}",
        String::from_utf8_lossy(&overridden.stderr)
    );
    assert!(overridden.stdout.is_empty());
    let stderr =
        String::from_utf8(overridden.stderr).expect("UTF-8 process logs");
    assert!(stderr.contains("starting Clawcode stdio transport"));
}

/// Real upstream calls record final identity/usage once without logging sensitive opted-in content.
#[tokio::test]
async fn rig_telemetry_logs_final_metadata_without_content() {
    use tracing::{Instrument, instrument::WithSubscriber};
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("info", false)
        .expect("filter")
        .build(logs.clone());
    async {
        for adopted in [false,true] {
            let trace=tracing::info_span!("acp_operation",trace_id=if adopted {"trace-adopted"} else {"trace-native"});
            async {
                let http=rig_core::test_utils::RecordingHttpClient::new(serde_json::json!({
                    "id":"reply-audit","object":"chat.completion","created":0,"model":"response-model",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"RESPONSE_SECRET_SENTINEL"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":1}}
                }).to_string());
                let model=rig_core::providers::openai::OpenAIConfig::new("API_SECRET_SENTINEL")
                    .with_route(rig_core::providers::openai::wire::Route::Chat).connect(http).completion("request-model");
                // These SDK fields exercise adoption and an explicitly enabled content recorder.
                let parent=adopted.then(|| rig_core::telemetry::completion_parent_span!(
                    target:"host::telemetry",name:"provider_call",operation:"chat",system_instructions:Some("SYSTEM_SECRET_SENTINEL"),authorization="AUTH_SECRET_SENTINEL"
                ));
                let request=rig_core::completion::CompletionRequest::new("PROMPT_SECRET_SENTINEL").record_content_telemetry(true);
                match parent {
                    Some(parent)=> {model.call(request).instrument(parent).await.expect("completion");}
                    None=> {model.call(request).await.expect("completion");}
                }
            }.instrument(trace).await;
        }
    }.with_subscriber(subscriber).await;
    let output = logs.content();
    let summaries = output
        .lines()
        .filter(|line| line.contains("Rig telemetry closed"))
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 2, "{output}");
    for summary in summaries {
        assert!(
            summary.contains("provider=\"openai\"")
                && summary.contains("request_model=\"request-model\"")
                && summary.contains("response_model=\"response-model\""),
            "{summary}"
        );
        assert!(
            summary.contains("input_tokens=12")
                && summary.contains("output_tokens=3")
                && summary.contains("cache_read_tokens=0")
                && summary.contains("reasoning_tokens=1"),
            "{summary}"
        );
        assert!(
            summary.contains("response_id=\"reply-audit\"")
                && summary.contains("elapsed_ms="),
            "{summary}"
        );
        assert!(!summary.contains("cache_write_tokens="));
    }
    assert!(
        output.contains("[trace-native]") && output.contains("[trace-adopted]"),
        "{output}"
    );
    for secret in [
        "PROMPT_SECRET_SENTINEL",
        "RESPONSE_SECRET_SENTINEL",
        "SYSTEM_SECRET_SENTINEL",
        "API_SECRET_SENTINEL",
        "AUTH_SECRET_SENTINEL",
    ] {
        assert!(!output.contains(secret), "content leaked: {secret}");
    }
}

/// Empty or interrupted spans retain unknown usage and emit only after their last handle closes.
#[test]
fn rig_telemetry_waits_for_close_and_preserves_unknown_usage() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("info", false)
        .expect("filter")
        .build(logs.clone());
    tracing::subscriber::with_default(subscriber, || {
        let span = rig_core::telemetry::SpanBuilder::new(
            "provider",
            "unreported",
            rig_core::telemetry::GenAiOperation::Chat,
        )
        .build();
        let other = span.clone();
        for _ in 0..3 {
            span.in_scope(|| {});
        }
        drop(span);
        assert!(
            logs.content().is_empty(),
            "polling/partial ownership must not log summaries"
        );
        drop(other);
        tracing::info_span!("ordinary_operation").in_scope(|| {});
    });
    let output = logs.content();
    assert_eq!(output.lines().count(), 1, "{output}");
    assert!(output.contains("Rig telemetry closed"), "{output}");
    assert!(
        !output.contains("input_tokens=") && !output.contains("output_tokens=")
    );
    assert!(
        !output.contains("succeeded") && !output.contains("ordinary_operation")
    );
}

/// Existing filters can disable Rig summaries without suppressing ordinary application logs.
#[test]
fn rig_telemetry_respects_existing_log_filters() {
    for (filter, expected) in [
        ("info,rig::completions=off", false),
        ("warn,rig::completions=info", true),
    ] {
        let logs = CapturedLogs::default();
        let subscriber = LoggingFactory::new(filter, false)
            .expect("filter")
            .build(logs.clone());
        tracing::subscriber::with_default(subscriber, || {
            drop(
                rig_core::telemetry::SpanBuilder::new(
                    "provider",
                    "model",
                    rig_core::telemetry::GenAiOperation::Chat,
                )
                .build(),
            );
            tracing::warn!("ordinary warning");
        });
        let output = logs.content();
        assert_eq!(
            output.contains("Rig telemetry closed"),
            expected,
            "{output}"
        );
        assert!(output.contains("ordinary warning"), "{output}");
    }
}
