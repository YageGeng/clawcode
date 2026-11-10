//! `BashExecutor`, which runs user `!` commands: output, exits, limits, and streaming.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use protocol::{SessionId, TurnId, UserBashResult};
use tokio_util::sync::CancellationToken;
use tools::{BashExecutionRequest, BashExecutor};

/// Runs one command and records every published output tail.
async fn run(
    cwd: &Path,
    command: &str,
    cancellation: CancellationToken,
    published: Arc<Mutex<Vec<String>>>,
) -> UserBashResult {
    let request = BashExecutionRequest::builder()
        .command(command.to_string())
        .cwd(cwd.to_path_buf())
        .session_id(SessionId::try_from("session-bash").expect("session id"))
        .turn_id(TurnId::try_from("turn-bash").expect("turn id"))
        .cancellation(cancellation)
        .build();
    BashExecutor::execute(request, move |output| {
        published.lock().expect("published").push(output);
    })
    .await
    .expect("execute")
}

/// Runs one command without cancellation or recording.
async fn run_plain(cwd: &Path, command: &str) -> UserBashResult {
    run(cwd, command, CancellationToken::new(), Arc::default()).await
}

/// Stdout and stderr are combined in arrival order.
#[tokio::test]
async fn executor_combines_output() {
    let root = tempfile::tempdir().expect("temporary directory");

    let result =
        run_plain(root.path(), "printf 'stdout'; printf 'stderr' >&2").await;

    assert_eq!(result.output, "stdoutstderr");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.cancelled);
}

/// A non-zero exit keeps the output and its code.
#[tokio::test]
async fn executor_reports_nonzero_exit_after_output() {
    let root = tempfile::tempdir().expect("temporary directory");

    let result = run_plain(root.path(), "printf 'before'; exit 7").await;

    assert_eq!(result.output, "before");
    assert_eq!(result.exit_code, Some(7));
}

/// A shell ended by a signal names it.
#[cfg(unix)]
#[tokio::test]
async fn executor_names_the_signal_that_ended_the_shell() {
    let root = tempfile::tempdir().expect("temporary directory");

    let result = run_plain(root.path(), "printf 'going'; kill -TERM $$").await;

    assert_eq!(result.exit_code, None);
    assert_eq!(result.signal.as_deref(), Some("SIGTERM"));
}

/// Cancellation kills the process group and keeps what it printed.
#[tokio::test]
async fn executor_observes_cancellation() {
    let root = tempfile::tempdir().expect("temporary directory");
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        run(
            root.path(),
            "printf 'running'; sleep 5",
            cancellation,
            Arc::default(),
        ),
    )
    .await
    .expect("cancellation settles");

    assert!(result.cancelled);
    assert_eq!(result.output, "running");
    #[cfg(unix)]
    assert_eq!(result.signal.as_deref(), Some("SIGKILL"));
}

/// Large output keeps the last 2000 lines and persists the complete stream.
#[tokio::test]
async fn executor_truncates_tail_and_persists_full_output() {
    let root = tempfile::tempdir().expect("temporary directory");

    let result =
        run_plain(root.path(), "for i in $(seq 1 3000); do echo $i; done")
            .await;

    let path = result
        .full_output_path
        .as_deref()
        .expect("full output path");
    let full = std::fs::read_to_string(path).expect("full output file");
    assert!(result.truncated);
    assert_eq!(result.output.lines().count(), 2_000);
    assert!(!result.output.lines().any(|line| line == "1"));
    assert!(result.output.lines().any(|line| line == "3000"));
    assert!(full.starts_with("1\n2\n3\n"));
    assert!(full.ends_with("2999\n3000\n"));
}

/// Streaming commands publish partial output before the final tail.
#[tokio::test]
async fn executor_publishes_partial_output() {
    let root = tempfile::tempdir().expect("temporary directory");
    let published = Arc::new(Mutex::new(Vec::new()));

    let result = run(
        root.path(),
        "printf 'first'; sleep 0.15; printf 'second'",
        CancellationToken::new(),
        Arc::clone(&published),
    )
    .await;

    assert_eq!(result.output, "firstsecond");
    let published = published.lock().expect("published");
    assert!(published.len() >= 2);
    assert!(published.iter().any(|output| output == "first"));
    assert_eq!(published.last().map(String::as_str), Some("firstsecond"));
}

/// Output from short-lived inherited pipes is drained after the shell exits.
#[tokio::test]
async fn executor_drains_inherited_stdio_within_exit_grace() {
    let root = tempfile::tempdir().expect("temporary directory");

    let result =
        run_plain(root.path(), "(sleep 0.2; printf 'late') & printf 'early'")
            .await;

    assert_eq!(result.output, "earlylate");
}
