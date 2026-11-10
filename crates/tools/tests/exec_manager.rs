#![cfg(unix)]
//! Process table, yield semantics, and lifecycle reporting of unified exec.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use protocol::{ExecExit, SessionId, ToolCall, ToolCallId, TurnId};
use tokio_util::sync::CancellationToken;
use tools::exec::{
    ExecCommandArgs, ExecError, ExecEventSink, ExecOutputUpdate,
    ExecProcessManager, ExecResponse, ExecSettings, ExecStart, ExecWrite,
    WriteStdinArgs, prune_candidate,
};

/// Records every lifecycle callback in arrival order.
#[derive(Default)]
struct RecordingSink {
    outputs: Mutex<Vec<ExecOutputUpdate>>,
    exits: Mutex<Vec<ExecExit>>,
}

#[async_trait]
impl ExecEventSink for RecordingSink {
    /// Stores one output snapshot.
    async fn output(&self, update: ExecOutputUpdate) {
        self.outputs.lock().expect("outputs").push(update);
    }

    /// Stores one exit report.
    async fn exited(&self, exit: ExecExit) {
        self.exits.lock().expect("exits").push(exit);
    }
}

impl RecordingSink {
    /// Number of exit reports so far.
    fn exit_count(&self) -> usize {
        self.exits.lock().expect("exits").len()
    }
}

/// One manager, its sink, and a scratch directory for a test.
struct Harness {
    manager: ExecProcessManager,
    sink: Arc<RecordingSink>,
    dir: tempfile::TempDir,
}

impl Harness {
    /// Creates a manager without shell snapshots.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("dir");
        let config =
            config::ExecConfig::builder().shell_snapshot(false).build();
        let manager = ExecProcessManager::new(Arc::new(ExecSettings {
            config,
            snapshot_dir: dir.path().join("snapshots"),
        }));
        Self {
            manager,
            sink: Arc::new(RecordingSink::default()),
            dir,
        }
    }

    /// Runs `exec_command` with literal arguments and the given cancellation.
    async fn exec(
        &self,
        arguments: serde_json::Value,
        cancellation: CancellationToken,
    ) -> Result<ExecResponse, ExecError> {
        let call = ToolCall::builder()
            .tool_call_id(ToolCallId::try_from("call-exec").expect("id"))
            .name("exec_command".to_string())
            .arguments(arguments)
            .build();
        self.manager
            .exec_command(
                ExecStart::builder()
                    .args(ExecCommandArgs::try_from(&call).expect("args"))
                    .cwd(self.dir.path().to_path_buf())
                    .tool_call_id(call.tool_call_id.clone())
                    .turn_id(TurnId::try_from("turn-exec").expect("turn"))
                    .session_id(
                        SessionId::try_from("session-exec").expect("id"),
                    )
                    .cancellation(cancellation)
                    .sink(Arc::clone(&self.sink) as Arc<dyn ExecEventSink>)
                    .build(),
            )
            .await
    }

    /// Runs a non-login `exec_command` for `cmd` with a yield time.
    async fn run(
        &self,
        cmd: &str,
        yield_ms: u64,
    ) -> Result<ExecResponse, ExecError> {
        self.exec(
            serde_json::json!({ "cmd": cmd, "yield_time_ms": yield_ms, "login": false }),
            CancellationToken::new(),
        )
        .await
    }

    /// Runs `write_stdin` against a session.
    async fn write(
        &self,
        session_id: i32,
        chars: &str,
        yield_ms: u64,
    ) -> Result<ExecResponse, ExecError> {
        self.write_with(session_id, chars, yield_ms, CancellationToken::new())
            .await
    }

    /// Runs `write_stdin` against a session with the given cancellation.
    async fn write_with(
        &self,
        session_id: i32,
        chars: &str,
        yield_ms: u64,
        cancellation: CancellationToken,
    ) -> Result<ExecResponse, ExecError> {
        let call = ToolCall::builder()
            .tool_call_id(ToolCallId::try_from("call-write").expect("id"))
            .name("write_stdin".to_string())
            .arguments(serde_json::json!({
                "session_id": session_id, "chars": chars, "yield_time_ms": yield_ms
            }))
            .build();
        self.manager
            .write_stdin(ExecWrite {
                args: WriteStdinArgs::try_from(&call).expect("args"),
                cancellation,
            })
            .await
    }

    /// Waits up to five seconds for `count` exit reports.
    async fn exits(&self, count: usize) {
        for _attempt in 0..100 {
            if self.sink.exit_count() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(self.sink.exit_count(), count, "exit reports");
    }
}

/// Returns the output section of a model text.
fn body(response: &ExecResponse) -> String {
    response
        .model_text()
        .split_once("\nOutput:\n")
        .map(|(_header, body)| body.to_string())
        .unwrap_or_default()
}

/// A command that finishes within the yield reports its exit and no session.
#[tokio::test]
async fn short_command_returns_exit_code_without_session_id() {
    let harness = Harness::new();

    let response = harness.run("echo hi", 2_000).await.expect("exec");

    assert_eq!(response.process_id, None);
    assert_eq!(response.exit_code, Some(0));
    assert!(body(&response).contains("hi"), "{}", response.model_text());
}

/// A slow command yields a session that a later poll completes.
#[tokio::test]
async fn long_command_yields_session_id_and_keeps_running() {
    let harness = Harness::new();

    let started = harness.run("sleep 1; echo late", 250).await.expect("exec");
    let session = started.process_id.expect("session id");
    let polled = harness.write(session, "", 5_000).await.expect("poll");
    let again = harness.write(session, "", 5_000).await;

    assert!(body(&polled).contains("late"), "{}", polled.model_text());
    assert_eq!(polled.exit_code, Some(0));
    assert_eq!(polled.process_id, None);
    assert!(
        matches!(again, Err(ExecError::UnknownProcessId(id)) if id == session)
    );
}

/// An exited session stays collectable but is no longer listed as running.
#[tokio::test]
async fn exited_background_process_waits_for_collection() {
    let harness = Harness::new();
    let session = harness
        .run("sleep 0.3; echo x", 250)
        .await
        .expect("exec")
        .process_id
        .expect("session id");
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(harness.manager.list().await.is_empty());
    let polled = harness.write(session, "", 5_000).await.expect("poll");
    assert!(body(&polled).contains('x'), "{}", polled.model_text());
    assert_eq!(polled.exit_code, Some(0));
}

/// Cancelling the first wait returns cancellation but keeps the process.
#[tokio::test]
async fn cancelled_wait_keeps_process() {
    let harness = Harness::new();
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });

    let result = harness
        .exec(
            serde_json::json!({ "cmd": "sleep 30", "yield_time_ms": 10_000, "login": false }),
            cancellation,
        )
        .await;

    assert!(matches!(result, Err(ExecError::Cancelled)));
    assert_eq!(harness.manager.list().await.len(), 1);
    assert_eq!(harness.manager.terminate_all().await, 1);
}

/// A background process streams snapshots and reports its exit once.
#[tokio::test]
async fn sink_receives_output_snapshots_and_one_exit() {
    let harness = Harness::new();

    let response = harness
        .run("for i in 1 2 3; do echo tick; sleep 0.2; done", 250)
        .await
        .expect("exec");
    harness.exits(1).await;

    assert!(response.process_id.is_some());
    let outputs = harness.sink.outputs.lock().expect("outputs").clone();
    assert!(outputs.iter().any(|update| update.output.contains("tick")));
    let exits = harness.sink.exits.lock().expect("exits").clone();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].tool_call_id.as_str(), "call-exec");
    assert_eq!(exits[0].exit_code, Some(0));
}

/// An exit returned by the first call is not reported again.
#[tokio::test]
async fn quick_exit_reports_through_result_only() {
    let harness = Harness::new();

    let response = harness.run("echo hi", 2_000).await.expect("exec");
    tokio::time::sleep(Duration::from_millis(700)).await;

    assert_eq!(response.exit_code, Some(0));
    assert_eq!(harness.sink.exit_count(), 0);
}

/// A process whose first wait was cancelled reports its later exit.
#[tokio::test]
async fn cancelled_process_reports_exit_later() {
    let harness = Harness::new();
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    });

    let result = harness
        .exec(
            serde_json::json!({ "cmd": "sleep 0.3", "login": false }),
            cancellation,
        )
        .await;
    harness.exits(1).await;

    assert!(matches!(result, Err(ExecError::Cancelled)));
}

/// A call cancelled before it starts spawns nothing.
#[tokio::test]
async fn cancelled_before_start_spawns_nothing() {
    let harness = Harness::new();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result = harness
        .exec(
            serde_json::json!({ "cmd": "sleep 30", "login": false }),
            cancellation,
        )
        .await;

    assert!(matches!(result, Err(ExecError::Cancelled)));
    assert!(harness.manager.list().await.is_empty());
}

/// Writing after a PTY process exited still collects its output and exit.
#[tokio::test]
async fn write_after_tty_exit_still_collects() {
    let harness = Harness::new();
    let session = harness
        .exec(
            serde_json::json!({ "cmd": "sleep 0.3; echo bye", "tty": true, "yield_time_ms": 250, "login": false }),
            CancellationToken::new(),
        )
        .await
        .expect("exec")
        .process_id
        .expect("session id");
    tokio::time::sleep(Duration::from_secs(1)).await;

    let response = harness.write(session, "x\n", 500).await.expect("write");

    assert!(body(&response).contains("bye"), "{}", response.model_text());
    assert_eq!(response.exit_code, Some(0));
}

/// Ctrl-C written to a pipe session interrupts it.
#[tokio::test]
async fn write_stdin_ctrl_c_interrupts_non_tty() {
    let harness = Harness::new();
    let session = harness
        .run("sleep 30", 250)
        .await
        .expect("exec")
        .process_id
        .expect("session id");

    let response = harness.write(session, "\u{3}", 2_000).await.expect("write");

    assert!(
        response.signal.as_deref() == Some("SIGINT")
            || response.exit_code == Some(130),
        "{}",
        response.model_text()
    );
}

/// Text written to a pipe session is rejected with codex's message.
#[tokio::test]
async fn write_stdin_text_to_non_tty_is_rejected() {
    let harness = Harness::new();
    let session = harness
        .run("sleep 30", 250)
        .await
        .expect("exec")
        .process_id
        .expect("session id");

    let error = harness
        .write(session, "y\n", 250)
        .await
        .expect_err("closed");

    assert_eq!(
        error.to_string(),
        "stdin is closed for this session; rerun exec_command with tty=true to keep stdin open"
    );
    harness.manager.terminate_all().await;
}

/// Cancelling a write to a tty child that never reads its input returns.
#[tokio::test]
async fn cancelled_write_to_unread_tty_returns() {
    let harness = Harness::new();
    let session = harness
        .exec(
            serde_json::json!({ "cmd": "stty -icanon -echo; sleep 30", "tty": true, "yield_time_ms": 250, "login": false }),
            CancellationToken::new(),
        )
        .await
        .expect("exec")
        .process_id
        .expect("session id");
    let cancellation = CancellationToken::new();
    let trigger = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });

    let written = tokio::time::timeout(
        Duration::from_secs(2),
        harness.write_with(
            session,
            &"x".repeat(128 * 1024),
            5_000,
            cancellation,
        ),
    )
    .await;

    harness.manager.terminate_all().await;
    assert!(
        matches!(written, Ok(Err(ExecError::Cancelled))),
        "{written:?}"
    );
}

/// A call cancelled before it runs sends none of its input.
#[tokio::test]
async fn already_cancelled_write_sends_no_input() {
    let harness = Harness::new();
    let session = harness
        .exec(
            serde_json::json!({ "cmd": "stty -echo; cat", "tty": true, "yield_time_ms": 250, "login": false }),
            CancellationToken::new(),
        )
        .await
        .expect("exec")
        .process_id
        .expect("session id");

    for attempt in 0..32 {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let written = harness
            .write_with(
                session,
                &format!("cancelled-{attempt}\n"),
                250,
                cancellation,
            )
            .await;
        assert!(matches!(written, Err(ExecError::Cancelled)), "{written:?}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    harness.manager.terminate_all().await;
    let outputs = harness.sink.outputs.lock().expect("outputs");
    assert!(
        !outputs
            .iter()
            .any(|update| update.output.contains("cancelled-")),
        "cancelled input reached the child"
    );
}

/// A call waiting for a session another call holds can be cancelled.
#[tokio::test]
async fn cancelled_write_waiting_for_busy_session_returns() {
    let harness = Harness::new();
    let session = harness
        .run("sleep 30", 250)
        .await
        .expect("exec")
        .process_id
        .expect("session id");
    let holder = CancellationToken::new();
    let waiter = CancellationToken::new();
    waiter.cancel();

    let (held, (queued, waited)) = tokio::join!(
        harness.write_with(session, "", 5_000, holder.clone()),
        async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let started = Instant::now();
            let queued = harness.write_with(session, "", 5_000, waiter).await;
            holder.cancel();
            (queued, started.elapsed())
        }
    );

    harness.manager.terminate_all().await;
    assert!(matches!(queued, Err(ExecError::Cancelled)), "{queued:?}");
    assert!(waited < Duration::from_secs(1), "waited {waited:?}");
    assert!(matches!(held, Err(ExecError::Cancelled)), "{held:?}");
}

/// Once a result carries the exit, no output snapshot follows it.
#[tokio::test]
async fn returned_exit_is_not_followed_by_output() {
    let harness = Harness::new();
    let response = harness
        .run("sleep 0.03; head -c 30000 /dev/zero | tr '\\0' a", 5_000)
        .await
        .expect("exec");
    assert_eq!(response.exit_code, Some(0));
    let returned = harness.sink.outputs.lock().expect("outputs").len();

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        harness.sink.outputs.lock().expect("outputs").len(),
        returned
    );
}

/// Eviction spares the eight most recent and prefers exited processes.
#[test]
fn prune_candidate_protects_eight_most_recent_and_prefers_exited() {
    let base = Instant::now();
    let meta = |exited_id: Option<i32>| -> Vec<(i32, Instant, bool)> {
        (0..10)
            .map(|index| {
                (
                    1000 + index,
                    base + Duration::from_secs(
                        u64::try_from(index).expect("index"),
                    ),
                    Some(1000 + index) == exited_id,
                )
            })
            .collect()
    };

    assert_eq!(prune_candidate(&meta(Some(1001))), Some(1001));
    assert_eq!(prune_candidate(&meta(None)), Some(1000));
    assert_eq!(prune_candidate(&meta(None)[2..]), None);
}

/// Terminating everything reports one exit per background process.
#[tokio::test]
async fn terminate_all_reports_every_exit() {
    let harness = Harness::new();
    for _process in 0..3 {
        harness.run("sleep 30", 250).await.expect("exec");
    }

    let terminated = harness.manager.terminate_all().await;

    assert_eq!(terminated, 3);
    assert_eq!(harness.sink.exit_count(), 3);
}

/// Floods stay bounded: truncated text, a full-output file, throttled snapshots.
#[tokio::test]
async fn flood_output_stays_bounded() {
    let harness = Harness::new();
    let started = Instant::now();

    let response = harness
        .run("head -c 3000000 /dev/zero | tr '\\0' a", 10_000)
        .await
        .expect("exec");

    assert!(response.model_text().contains("tokens truncated"));
    let path: &PathBuf =
        response.full_output_path.as_ref().expect("full output");
    let kept = std::fs::metadata(path).expect("full output file").len();
    std::fs::remove_file(path).expect("remove full output");
    assert_eq!(kept, 3_000_000, "full output keeps every byte");
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).expect("ms");
    let outputs = harness.sink.outputs.lock().expect("outputs").len();
    assert!(
        u64::try_from(outputs).expect("count") <= elapsed_ms / 100 + 2,
        "{outputs} snapshots in {elapsed_ms} ms"
    );
}

/// A background exit points at a file holding every byte the process printed.
#[tokio::test]
async fn background_exit_keeps_every_byte() {
    let harness = Harness::new();
    harness
        .run("sleep 0.5; head -c 2000000 /dev/zero | tr '\\0' a", 250)
        .await
        .expect("exec")
        .process_id
        .expect("session id");

    harness.exits(1).await;

    let exit = harness.sink.exits.lock().expect("exits").remove(0);
    let path = exit.full_output_path.expect("full output");
    let kept = std::fs::metadata(&path).expect("full output file").len();
    std::fs::remove_file(&path).expect("remove full output");
    assert_eq!(kept, 2_000_000);
}

/// Two concurrent polls see the output and exit only once.
#[tokio::test]
async fn concurrent_polls_return_output_once() {
    let harness = Harness::new();
    let session = harness
        .run("sleep 0.3; echo once", 250)
        .await
        .expect("exec")
        .process_id
        .expect("session id");

    let (first, second) = tokio::join!(
        harness.write(session, "", 5_000),
        harness.write(session, "", 5_000)
    );

    let results = [first, second];
    let texts: String = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .map(body)
        .collect();
    assert_eq!(texts.matches("once").count(), 1, "{texts}");
    let exits = results
        .iter()
        .filter(|result| matches!(result, Ok(response) if response.exit_code.is_some()))
        .count();
    assert_eq!(exits, 1);
}

/// Relative workdirs resolve against the cwd; missing ones fail before spawning.
#[tokio::test]
async fn relative_workdir_resolves_and_missing_is_error() {
    let harness = Harness::new();
    std::fs::create_dir(harness.dir.path().join("sub")).expect("sub");

    let response = harness
        .exec(
            serde_json::json!({ "cmd": "pwd", "workdir": "sub", "login": false }),
            CancellationToken::new(),
        )
        .await
        .expect("exec");
    let missing = harness
        .exec(
            serde_json::json!({ "cmd": "pwd", "workdir": "missing", "login": false }),
            CancellationToken::new(),
        )
        .await;

    assert!(
        body(&response).trim_end().ends_with("/sub"),
        "{}",
        response.model_text()
    );
    assert!(
        matches!(missing, Err(ExecError::InvalidWorkdir(path)) if path.ends_with(Path::new("missing")))
    );
    assert!(harness.manager.list().await.is_empty());
}
