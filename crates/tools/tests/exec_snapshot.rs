#![cfg(unix)]
//! Shell snapshots: login-shell state captured once and replayed per command.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use protocol::{ExecExit, SessionId, ToolCall, ToolCallId, TurnId};
use tokio_util::sync::CancellationToken;
use tools::exec::{
    ExecCommandArgs, ExecEnvironment, ExecEventSink, ExecOutputUpdate,
    ExecProcessManager, ExecSettings, ExecStart, ShellSnapshot, UserShell,
};

/// Environment of a bash whose home holds `profile` as its login profile.
fn home_with_profile(
    profile: &str,
) -> (tempfile::TempDir, BTreeMap<String, String>) {
    let home = tempfile::tempdir().expect("home");
    std::fs::write(home.path().join(".bash_profile"), profile)
        .expect("profile");
    let env = BTreeMap::from([
        ("HOME".to_string(), home.path().display().to_string()),
        (
            "PATH".to_string(),
            std::env::var("PATH")
                .unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
        ),
    ]);
    (home, env)
}

/// Runs a rewritten command line and returns its stdout.
async fn run(
    argv: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> String {
    let (program, args) = argv.split_first().expect("program");
    let output = tokio::process::Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .expect("run");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Functions and aliases from the login profile work in replayed commands.
#[tokio::test]
async fn snapshot_restores_functions_and_aliases() {
    let (home, env) = home_with_profile(
        "greet() { echo hi-from-rc; }\nalias ll='echo ll-alias'\n",
    );
    let snapshot = ShellSnapshot::new(
        UserShell::from(PathBuf::from("/bin/bash")),
        home.path().join("snapshot.sh"),
    );

    snapshot
        .capture(
            home.path(),
            &env,
            &ExecEnvironment::new(&config::ExecEnvPolicy::default()),
            Duration::from_secs(10),
        )
        .await
        .expect("capture");
    let argv = snapshot.shell().argv(
        "greet; ll",
        true,
        Some(snapshot.path()),
        &BTreeMap::new(),
    );
    let output = run(&argv, &env, home.path()).await;

    let state = std::fs::read_to_string(snapshot.path()).unwrap_or_default();
    assert!(
        output.contains("hi-from-rc"),
        "{output}\n--- snapshot ---\n{state}"
    );
    assert!(
        output.contains("ll-alias"),
        "{output}\n--- snapshot ---\n{state}"
    );
    assert_eq!(argv[1], "-c", "replay skips the login startup");
}

/// Forced variables win over the ones the snapshot restores.
#[tokio::test]
async fn snapshot_does_not_override_forced_env() {
    let (home, env) = home_with_profile("export PAGER=less\n");
    let snapshot = ShellSnapshot::new(
        UserShell::from(PathBuf::from("/bin/bash")),
        home.path().join("snapshot.sh"),
    );
    snapshot
        .capture(
            home.path(),
            &env,
            &ExecEnvironment::new(&config::ExecEnvPolicy::default()),
            Duration::from_secs(10),
        )
        .await
        .expect("capture");

    let argv = snapshot.shell().argv(
        "echo pager=$PAGER",
        true,
        Some(snapshot.path()),
        &BTreeMap::from([("PAGER".to_string(), "cat".to_string())]),
    );
    let output = run(&argv, &env, home.path()).await;

    assert!(output.contains("pager=cat"), "{output}");
}

/// Without a snapshot the command runs as a plain login shell.
#[test]
fn argv_without_snapshot_is_plain_login() {
    let shell = UserShell::from(PathBuf::from("/bin/bash"));

    assert_eq!(
        shell.argv("echo hi", true, None, &BTreeMap::new()),
        ["/bin/bash", "-lc", "echo hi"]
    );
}

/// Cleanup removes snapshot files older than the limit and keeps fresh ones.
#[tokio::test]
async fn cleanup_removes_only_stale_files() {
    let dir = tempfile::tempdir().expect("dir");
    let stale = dir.path().join("old.sh");
    let fresh = dir.path().join("new.sh");
    std::fs::write(&stale, "").expect("stale");
    std::fs::write(&fresh, "").expect("fresh");
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .expect("open stale")
        .set_modified(SystemTime::now() - Duration::from_secs(4 * 24 * 60 * 60))
        .expect("age stale");

    ShellSnapshot::cleanup_stale(
        dir.path(),
        Duration::from_secs(3 * 24 * 60 * 60),
    )
    .await;

    assert!(!stale.exists());
    assert!(fresh.exists());
}

/// A login profile that hangs fails the capture at its time limit.
#[tokio::test]
async fn capture_times_out() {
    let (home, env) = home_with_profile("sleep 30\n");
    let snapshot = ShellSnapshot::new(
        UserShell::from(PathBuf::from("/bin/bash")),
        home.path().join("snapshot.sh"),
    );
    let started = std::time::Instant::now();

    let result = snapshot
        .capture(
            home.path(),
            &env,
            &ExecEnvironment::new(&config::ExecEnvPolicy::default()),
            Duration::from_secs(1),
        )
        .await;

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!snapshot.path().exists());
}

/// Ignores lifecycle reports.
struct NoSink;

#[async_trait]
impl ExecEventSink for NoSink {
    /// Drops one snapshot.
    async fn output(&self, _update: ExecOutputUpdate) {}
    /// Drops one exit.
    async fn exited(&self, _exit: ExecExit) {}
}

/// The first command of a Session starts generation; removal deletes the file.
#[tokio::test]
async fn snapshot_is_generated_on_first_exec_only() {
    let dir = tempfile::tempdir().expect("dir");
    let snapshots = dir.path().join("snapshots");
    let manager = ExecProcessManager::new(Arc::new(ExecSettings {
        config: config::ExecConfig::default(),
        snapshot_dir: snapshots.clone(),
    }));
    let expected = snapshots.join("session-snap.sh");
    assert!(!expected.exists(), "nothing before the first command");
    let call = ToolCall::builder()
        .tool_call_id(ToolCallId::try_from("call-snap").expect("id"))
        .name("exec_command".to_string())
        .arguments(serde_json::json!({ "cmd": "true", "shell": "/bin/bash" }))
        .build();

    manager
        .exec_command(
            ExecStart::builder()
                .args(ExecCommandArgs::try_from(&call).expect("args"))
                .cwd(dir.path().to_path_buf())
                .tool_call_id(call.tool_call_id.clone())
                .turn_id(TurnId::try_from("turn-snap").expect("turn"))
                .session_id(SessionId::try_from("session-snap").expect("id"))
                .cancellation(CancellationToken::new())
                .sink(Arc::new(NoSink) as Arc<dyn ExecEventSink>)
                .build(),
        )
        .await
        .expect("exec");
    for _attempt in 0..100 {
        if expected.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(expected.exists(), "snapshot generated");
    manager.remove_snapshot().await;
    assert!(!expected.exists(), "snapshot removed");
}

/// Removing a snapshot still being generated stops it; no file appears later.
#[tokio::test]
async fn removing_pending_snapshot_prevents_recreation() {
    let (home, _env) = home_with_profile("sleep 0.5\n");
    let snapshots = home.path().join("snapshots");
    let policy = config::ExecEnvPolicy::builder()
        .set(BTreeMap::from([(
            "HOME".to_string(),
            home.path().display().to_string(),
        )]))
        .build();
    let manager = ExecProcessManager::new(Arc::new(ExecSettings {
        config: config::ExecConfig::builder().env(policy).build(),
        snapshot_dir: snapshots.clone(),
    }));
    let call = ToolCall::builder()
        .tool_call_id(ToolCallId::try_from("call-pending").expect("id"))
        .name("exec_command".to_string())
        .arguments(serde_json::json!({ "cmd": "true", "shell": "/bin/bash", "login": false }))
        .build();
    manager
        .exec_command(
            ExecStart::builder()
                .args(ExecCommandArgs::try_from(&call).expect("args"))
                .cwd(home.path().to_path_buf())
                .tool_call_id(call.tool_call_id.clone())
                .turn_id(TurnId::try_from("turn-pending").expect("turn"))
                .session_id(SessionId::try_from("session-pending").expect("id"))
                .cancellation(CancellationToken::new())
                .sink(Arc::new(NoSink) as Arc<dyn ExecEventSink>)
                .build(),
        )
        .await
        .expect("exec");

    manager.remove_snapshot().await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        !snapshots.join("session-pending.sh").exists(),
        "snapshot written after removal"
    );
}

/// zsh snapshots restore functions and aliases from `.zshrc` as well.
#[tokio::test]
async fn zsh_snapshot_restores_functions_and_aliases() {
    if !Path::new("/bin/zsh").exists() {
        return;
    }
    let home = tempfile::tempdir().expect("home");
    std::fs::write(
        home.path().join(".zshrc"),
        "greet() { echo hi-from-zshrc; }\nalias ll='echo zsh-alias'\n",
    )
    .expect("zshrc");
    let env = BTreeMap::from([
        ("HOME".to_string(), home.path().display().to_string()),
        ("ZDOTDIR".to_string(), home.path().display().to_string()),
        (
            "PATH".to_string(),
            std::env::var("PATH")
                .unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
        ),
    ]);
    let snapshot = ShellSnapshot::new(
        UserShell::from(PathBuf::from("/bin/zsh")),
        home.path().join("snapshot.sh"),
    );

    snapshot
        .capture(
            home.path(),
            &env,
            &ExecEnvironment::new(&config::ExecEnvPolicy::default()),
            Duration::from_secs(10),
        )
        .await
        .expect("capture");
    let argv = snapshot.shell().argv(
        "greet; ll",
        true,
        Some(snapshot.path()),
        &BTreeMap::new(),
    );
    let output = run(&argv, &env, home.path()).await;

    let state = std::fs::read_to_string(snapshot.path()).unwrap_or_default();
    assert!(
        output.contains("hi-from-zshrc"),
        "{output}\n--- snapshot ---\n{state}"
    );
    assert!(
        output.contains("zsh-alias"),
        "{output}\n--- snapshot ---\n{state}"
    );
}

/// Exports the policy removes never reach the snapshot, and the file is private.
#[tokio::test]
async fn snapshot_respects_env_policy_and_is_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let (home, env) =
        home_with_profile("export SECRET_TOKEN=s3cr3t\nexport KEEP_ME=1\n");
    let path = home.path().join("snapshots").join("snapshot.sh");
    let snapshot = ShellSnapshot::new(
        UserShell::from(PathBuf::from("/bin/bash")),
        path.clone(),
    );
    let policy = config::ExecEnvPolicy::builder()
        .ignore_default_excludes(false)
        .build();

    snapshot
        .capture(
            home.path(),
            &env,
            &ExecEnvironment::new(&policy),
            Duration::from_secs(10),
        )
        .await
        .expect("capture");

    let state = std::fs::read_to_string(&path).expect("snapshot");
    assert!(!state.contains("SECRET_TOKEN"), "{state}");
    assert!(state.contains("KEEP_ME"), "{state}");
    let file_mode =
        std::fs::metadata(&path).expect("file").permissions().mode() & 0o777;
    let dir_mode = std::fs::metadata(path.parent().expect("dir"))
        .expect("dir")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600);
    assert_eq!(dir_mode, 0o700);
}
