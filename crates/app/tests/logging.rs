use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use app::LoggingFactory;
use protocol::{ProductIdentity, RUN_ID_FIELD};

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

/// Allowed color uses tracing's ANSI styling, which a non-empty `NO_COLOR` turns off.
#[test]
fn logging_factory_emits_ansi_when_color_is_enabled() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("info", true)
        .expect("valid filter")
        .build(logs.clone());

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("colored diagnostic");
    });

    // Same reading as tracing: a non-Unicode value counts as unset.
    let no_color =
        std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty());
    assert_eq!(logs.content().contains("\u{1b}["), !no_color);
}

/// Writes a config for the production binary with the given `[logging]` table.
fn logging_config(
    directory: &std::path::Path,
    logging: &str,
) -> std::path::PathBuf {
    let config_path = directory.join("claw.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
active_model = "fixture/model"

[logging]
{logging}

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
"#
        ),
    )
    .expect("write logging config");
    config_path
}

/// Allowed color is still off when stderr is redirected away from a terminal.
#[test]
fn redirected_process_logs_have_no_ansi_even_with_color() {
    let directory = tempfile::tempdir().expect("logging config directory");
    let config_path =
        logging_config(directory.path(), "filter = \"info\"\ncolor = true");

    let output = Command::new(env!("CARGO_BIN_EXE_clawcode"))
        .arg("stdio")
        .env(ProductIdentity::CONFIG_PATH_ENV, &config_path)
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .output()
        .expect("run colored logging process");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 process logs");
    assert!(
        stderr.contains("starting Clawcode stdio transport"),
        "{stderr}"
    );
    assert!(!stderr.contains("\u{1b}["), "{stderr}");
}

/// Process startup uses the TOML filter unless `RUST_LOG` explicitly overrides it.
#[test]
fn process_logging_prefers_environment_over_toml_filter() {
    let directory = tempfile::tempdir().expect("logging config directory");
    let config_path = logging_config(directory.path(), "filter = \"off\"");

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

/// Returns the first captured line that contains `needle`, or the output as the error.
fn line<'a>(output: &'a str, needle: &str) -> Result<&'a str, String> {
    output
        .lines()
        .find(|line| line.contains(needle))
        .ok_or_else(|| format!("missing {needle} in {output}"))
}

/// Opening and closing spans, including Rig's, writes nothing on its own.
#[test]
fn logging_factory_writes_no_span_lifecycle_lines() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("trace", false)
        .expect("valid filter")
        .build(logs.clone());

    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!(target: "rig::completions", "chat").in_scope(|| {});
        tracing::error_span!("run", { RUN_ID_FIELD } = "run-x").in_scope(|| {});
    });

    assert_eq!(logs.content(), "");
}

/// Warnings inside a Run keep its span fields even when the filter hides INFO.
#[tokio::test]
async fn run_span_prefix_survives_a_warn_filter() {
    let logs = CapturedLogs::default();
    let subscriber = LoggingFactory::new("warn", false)
        .expect("valid filter")
        .build(logs.clone());
    let _guard = tracing::subscriber::set_default(subscriber);
    let trace = protocol::TraceId::try_from("trace-w").expect("trace");
    let run = protocol::RunId::try_from("run-w").expect("run");

    kernel::RunSpan::top_level(&trace, &run)
        .instrument(async { tracing::warn!("slow tool") })
        .await;

    assert!(
        line(&logs.content(), "slow tool")
            .expect("log line")
            .contains("run{trace_id=trace-w run_id=run-w}: ")
    );
}
