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
