use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use protocol::{
    ContentBlock, SessionId, ToolCall, ToolCallId, ToolResult,
    ToolResultDetails, TurnId,
};
use tokio_util::sync::CancellationToken;
use tools::{
    BuiltinToolFactory, ToolExecutionContext, ToolFactory, ToolUpdateSink,
};

/// Records replaceable streaming snapshots in publication order.
#[derive(Default)]
struct RecordingUpdates(Mutex<Vec<ToolResult>>);

impl ToolUpdateSink for RecordingUpdates {
    /// Stores one partial bash snapshot.
    fn publish(&self, update: ToolResult) {
        self.0.lock().expect("updates lock").push(update);
    }
}

/// Builds a direct bash context with caller-owned cancellation and updates.
fn execution_context(
    cwd: &Path,
    cancellation: CancellationToken,
    updates: Arc<dyn ToolUpdateSink>,
) -> ToolExecutionContext {
    ToolExecutionContext::builder()
        .session_id(SessionId::try_from("session-bash").expect("session id"))
        .turn_id(TurnId::try_from("turn-bash").expect("turn id"))
        .cwd(cwd.to_path_buf())
        .cancellation(cancellation)
        .updates(updates)
        .build()
}

/// Builds one bash call with an optional timeout in seconds.
fn bash_call(id: &str, command: &str, timeout: Option<f64>) -> ToolCall {
    let mut arguments = serde_json::Map::from_iter([(
        "command".to_string(),
        serde_json::json!(command),
    )]);
    if let Some(timeout) = timeout {
        arguments.insert("timeout".to_string(), serde_json::json!(timeout));
    }
    ToolCall {
        tool_call_id: ToolCallId::try_from(id).expect("tool call id"),
        name: "bash".to_string(),
        arguments: serde_json::Value::Object(arguments),
    }
}

/// Returns the default pi-compatible built-in registry.
fn registry() -> tools::ToolRegistry {
    BuiltinToolFactory::new()
        .create()
        .expect("built-in registry")
}

/// Bash returns combined output and uses pi's no-output placeholder.
#[tokio::test]
async fn bash_executes_commands_and_formats_empty_output() {
    let root = tempfile::tempdir().expect("temporary directory");
    let updates: Arc<dyn ToolUpdateSink> =
        Arc::new(RecordingUpdates::default());
    let context =
        execution_context(root.path(), CancellationToken::new(), updates);

    let output = registry()
        .execute(
            bash_call(
                "bash-output",
                "printf 'stdout'; printf 'stderr' >&2",
                None,
            ),
            &context,
        )
        .await
        .expect("execute output command");
    let empty = registry()
        .execute(bash_call("bash-empty", ":", None), &context)
        .await
        .expect("execute empty command");

    assert_eq!(output.blocks[0].text(), Some("stdoutstderr"));
    assert_eq!(empty.blocks[0].text(), Some("(no output)"));
}

/// Non-zero exit errors retain emitted output and the exact status footer.
#[tokio::test]
async fn bash_reports_nonzero_exit_after_output() {
    let root = tempfile::tempdir().expect("temporary directory");
    let error = registry()
        .execute(
            bash_call("bash-fail", "printf 'before'; exit 7", None),
            &execution_context(
                root.path(),
                CancellationToken::new(),
                Arc::new(RecordingUpdates::default()),
            ),
        )
        .await
        .expect_err("nonzero command should fail");

    assert!(
        error
            .to_string()
            .contains("before\n\nCommand exited with code 7")
    );
}

/// Timeout rejects non-positive values and terminates long-running process groups.
#[tokio::test]
async fn bash_validates_and_enforces_timeout_seconds() {
    let root = tempfile::tempdir().expect("temporary directory");
    let context = execution_context(
        root.path(),
        CancellationToken::new(),
        Arc::new(RecordingUpdates::default()),
    );
    let invalid = registry()
        .execute(bash_call("bash-invalid-timeout", ":", Some(0.0)), &context)
        .await
        .expect_err("zero timeout should fail");
    assert!(
        invalid
            .to_string()
            .contains("Invalid timeout: must be a finite number of seconds")
    );

    let timed_out = tokio::time::timeout(
        Duration::from_secs(2),
        registry().execute(
            bash_call("bash-timeout", "printf 'started'; sleep 5", Some(0.1)),
            &context,
        ),
    )
    .await
    .expect("tool timeout should settle")
    .expect_err("command should time out");
    assert!(
        timed_out
            .to_string()
            .contains("started\n\nCommand timed out after 0.1 seconds")
    );
}

/// Turn cancellation terminates bash and reports output captured before cancellation.
#[tokio::test]
async fn bash_observes_turn_cancellation() {
    let root = tempfile::tempdir().expect("temporary directory");
    let cancellation = CancellationToken::new();
    let context = execution_context(
        root.path(),
        cancellation.clone(),
        Arc::new(RecordingUpdates::default()),
    );
    let registry = registry();
    let task = tokio::spawn(async move {
        registry
            .execute(
                bash_call("bash-cancel", "printf 'running'; sleep 5", None),
                &context,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancellation.cancel();

    let error = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("cancelled tool should settle")
        .expect("bash task")
        .expect_err("cancelled command should fail");
    assert!(error.to_string().contains("running\n\nCommand aborted"));
}

/// Large output keeps the last 2000 lines and persists the complete stream.
#[tokio::test]
async fn bash_truncates_tail_and_persists_full_output() {
    let root = tempfile::tempdir().expect("temporary directory");
    let result = registry()
        .execute(
            bash_call(
                "bash-large",
                "for i in $(seq 1 3000); do echo $i; done",
                None,
            ),
            &execution_context(
                root.path(),
                CancellationToken::new(),
                Arc::new(RecordingUpdates::default()),
            ),
        )
        .await
        .expect("large command");
    let text = result.blocks[0].text().expect("bash output");
    let Some(ToolResultDetails::Bash {
        truncation: Some(truncation),
        full_output_path: Some(full_output_path),
    }) = result.details
    else {
        panic!("bash truncation details");
    };
    let full_output =
        std::fs::read_to_string(&full_output_path).expect("full output file");

    assert!(!text.lines().any(|line| line == "1"));
    assert!(text.lines().any(|line| line == "3000"));
    assert!(text.contains("[Showing lines 1001-3000 of 3000. Full output:"));
    assert_eq!(truncation.output_lines, 2_000);
    assert!(full_output.starts_with("1\n2\n3\n"));
    assert!(full_output.ends_with("2999\n3000\n"));
}

/// Streaming commands publish an initial snapshot and throttled visible output.
#[tokio::test]
async fn bash_publishes_partial_output_updates() {
    let root = tempfile::tempdir().expect("temporary directory");
    let updates = Arc::new(RecordingUpdates::default());
    let result = registry()
        .execute(
            bash_call(
                "bash-updates",
                "printf 'first'; sleep 0.15; printf 'second'",
                None,
            ),
            &execution_context(
                root.path(),
                CancellationToken::new(),
                Arc::clone(&updates) as Arc<dyn ToolUpdateSink>,
            ),
        )
        .await
        .expect("streaming command");
    let snapshots = updates.0.lock().expect("updates lock");

    assert_eq!(result.blocks[0].text(), Some("firstsecond"));
    assert!(snapshots.len() >= 2);
    assert!(
        snapshots
            .first()
            .is_some_and(|snapshot| snapshot.blocks.is_empty())
    );
    assert!(snapshots.iter().any(|snapshot| {
        snapshot
            .blocks
            .iter()
            .filter_map(ContentBlock::text)
            .any(|text| text.contains("first"))
    }));
}

/// Bash drains output from short-lived inherited pipes after the shell exits.
#[tokio::test]
async fn bash_drains_inherited_stdio_within_exit_grace() {
    let root = tempfile::tempdir().expect("temporary directory");
    let result = registry()
        .execute(
            bash_call(
                "bash-inherited-stdio",
                "(sleep 0.2; printf 'late') & printf 'early'",
                None,
            ),
            &execution_context(
                root.path(),
                CancellationToken::new(),
                Arc::new(RecordingUpdates::default()),
            ),
        )
        .await
        .expect("command with inherited stdout");

    assert_eq!(result.blocks[0].text(), Some("earlylate"));
}
