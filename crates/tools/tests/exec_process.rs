#![cfg(unix)]
//! Process spawning, I/O, and process-group control of unified exec.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use tools::exec::{ExecError, ExecProcess, ProcessState, SpawnSpec};

/// Spawns `/bin/sh -c script` in `cwd` with the parent environment.
fn spawn(cwd: &Path, script: &str, tty: bool) -> ExecProcess {
    ExecProcess::spawn(
        SpawnSpec::builder()
            .argv(vec!["/bin/sh".into(), "-c".into(), script.into()])
            .cwd(cwd.to_path_buf())
            .env(std::env::vars().collect::<BTreeMap<_, _>>())
            .tty(tty)
            .build(),
    )
    .expect("spawn")
}

/// Waits for exit and closed output, failing the test after `limit`.
async fn finished(process: &ExecProcess, limit: Duration) -> ProcessState {
    tokio::time::timeout(limit, process.wait_finished())
        .await
        .expect("process finished in time")
}

/// Returns the whole transcript decoded as text.
fn transcript(process: &ExecProcess) -> String {
    process.buffers().transcript.text()
}

/// Polls until `check` holds or two seconds pass.
async fn eventually(check: impl Fn() -> bool) -> bool {
    for _attempt in 0..40 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    check()
}

/// A pipe process reports its exit code and the output it printed.
#[tokio::test]
async fn pipe_process_reports_exit_code_and_output() {
    let dir = tempfile::tempdir().expect("dir");
    let process = spawn(dir.path(), "printf hi; exit 3", false);

    let state = finished(&process, Duration::from_secs(5)).await;

    assert_eq!(state.exit_code, Some(3));
    assert_eq!(transcript(&process), "hi");
}

/// `setsid` detaches pipe processes, so opening `/dev/tty` fails at once.
#[tokio::test]
async fn pipe_process_has_no_controlling_terminal() {
    let dir = tempfile::tempdir().expect("dir");
    let process = spawn(dir.path(), "read x </dev/tty; echo rc=$?", false);

    finished(&process, Duration::from_secs(2)).await;

    let output = transcript(&process);
    assert!(output.contains("rc="), "{output}");
    assert!(!output.contains("rc=0"), "{output}");
}

/// Terminating kills grandchildren that share the process group.
#[tokio::test]
async fn terminate_kills_the_whole_process_group() {
    let dir = tempfile::tempdir().expect("dir");
    let pid_file = dir.path().join("pid");
    let process = spawn(
        dir.path(),
        &format!("sleep 300 & echo $! > '{}'; wait", pid_file.display()),
        false,
    );
    assert!(eventually(|| pid_file.exists()).await, "pid file written");
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("pid")
        .trim()
        .parse()
        .expect("numeric pid");

    process.terminate();

    let gone = eventually(|| {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err()
    })
    .await;
    assert!(gone, "grandchild {pid} still alive");
}

/// A PTY process reads what is written to it.
#[tokio::test]
async fn pty_process_echoes_written_input() {
    let dir = tempfile::tempdir().expect("dir");
    let process = spawn(dir.path(), "cat", true);

    process.write(b"hello\n").expect("write");

    assert!(
        eventually(|| transcript(&process).contains("hello")).await,
        "{}",
        transcript(&process)
    );
    process.terminate();
}

/// A PTY process sees a terminal on stdin.
#[tokio::test]
async fn pty_is_a_terminal() {
    let dir = tempfile::tempdir().expect("dir");
    let process = spawn(dir.path(), "[ -t 0 ] && echo yes", true);

    finished(&process, Duration::from_secs(5)).await;

    assert!(
        transcript(&process).contains("yes"),
        "{}",
        transcript(&process)
    );
}

/// Writing to a pipe process fails because its stdin is closed.
#[tokio::test]
async fn pipe_process_rejects_stdin_writes() {
    let dir = tempfile::tempdir().expect("dir");
    let process = spawn(dir.path(), "sleep 5", false);

    let error = process.write(b"y\n").expect_err("stdin closed");

    assert!(matches!(error, ExecError::StdinClosed), "{error}");
    process.terminate();
}

/// An interrupt reaches the shell as SIGINT.
#[tokio::test]
async fn interrupt_sends_sigint_to_group() {
    let dir = tempfile::tempdir().expect("dir");
    let process = spawn(
        dir.path(),
        "trap 'echo got-int; exit 130' INT; sleep 30 & wait $!",
        false,
    );
    tokio::time::sleep(Duration::from_millis(300)).await;

    process.interrupt().expect("interrupt");
    let state = finished(&process, Duration::from_secs(5)).await;

    assert!(
        transcript(&process).contains("got-int"),
        "{}",
        transcript(&process)
    );
    assert_eq!(state.exit_code, Some(130));
    process.terminate();
}

/// Invalid UTF-8 output is decoded lossily instead of failing.
#[tokio::test]
async fn invalid_utf8_output_is_lossy_not_fatal() {
    let dir = tempfile::tempdir().expect("dir");
    let process = spawn(dir.path(), "printf '\\377\\376 ok'", false);

    finished(&process, Duration::from_secs(5)).await;

    let output = transcript(&process);
    assert!(output.contains("ok"), "{output}");
    assert!(output.contains('\u{FFFD}'), "{output}");
}

/// A detached child holding the pipes does not keep the process unfinished.
#[tokio::test]
async fn background_child_holding_pipes_does_not_block_exit() {
    let dir = tempfile::tempdir().expect("dir");
    let process = spawn(dir.path(), "(sleep 30 &); echo done", false);

    let state = finished(&process, Duration::from_secs(2)).await;

    assert!(state.exited);
    assert!(
        transcript(&process).contains("done"),
        "{}",
        transcript(&process)
    );
    process.terminate();
}

/// Once the leader is reaped and its group is empty, the group is never signalled again.
#[tokio::test]
async fn exited_process_with_empty_group_is_not_signalled() {
    let dir = tempfile::tempdir().expect("dir");
    let process = spawn(dir.path(), "true", false);
    finished(&process, Duration::from_secs(5)).await;

    // An empty group's id may already name an unrelated group; signalling it must be skipped.
    assert!(
        process.interrupt().is_ok(),
        "interrupt after exit is a no-op"
    );
}

/// A daemon left behind by an exited leader is still killed with its group.
#[tokio::test]
async fn daemon_left_by_exited_leader_is_killed_on_terminate() {
    let dir = tempfile::tempdir().expect("dir");
    let pid_file = dir.path().join("pid");
    let process = spawn(
        dir.path(),
        &format!(
            "sleep 300 >/dev/null 2>&1 & echo $! > '{}'",
            pid_file.display()
        ),
        false,
    );
    finished(&process, Duration::from_secs(5)).await;
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("pid")
        .trim()
        .parse()
        .expect("numeric pid");

    process.terminate();

    let gone = eventually(|| {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err()
    })
    .await;
    assert!(gone, "daemon {pid} still alive");
}
