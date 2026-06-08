use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

use tokio::io::AsyncWriteExt;

use super::ConfiguredHandler;

/// Result of one command hook process.
pub(crate) struct CommandRunResult {
    /// Unix timestamp in seconds when the command started.
    pub(crate) started_at: i64,
    /// Unix timestamp in seconds when the command completed.
    pub(crate) completed_at: i64,
    /// Runtime duration in milliseconds.
    pub(crate) duration_ms: i64,
    /// Process exit code.
    pub(crate) exit_code: Option<i32>,
    /// Captured stdout.
    pub(crate) stdout: String,
    /// Captured stderr.
    pub(crate) stderr: String,
    /// Spawn or IO error.
    pub(crate) error: Option<String>,
}

/// Execute a hook command with JSON input on stdin.
pub(crate) async fn run_command(
    handler: &ConfiguredHandler,
    input_json: &str,
    cwd: PathBuf,
) -> CommandRunResult {
    let started_at = chrono::Utc::now().timestamp();
    let started = Instant::now();
    let mut command = default_shell_command();
    command
        .arg(&handler.command)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return command_error_result(
                started_at,
                started,
                error.to_string(),
            );
        }
    };
    if let Some(mut stdin) = child.stdin.take()
        && let Err(error) = stdin.write_all(input_json.as_bytes()).await
    {
        let _ = child.kill().await;
        return command_error_result(
            started_at,
            started,
            format!("failed to write hook stdin: {error}"),
        );
    }
    match tokio::time::timeout(
        Duration::from_secs(handler.timeout_sec),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(output)) => CommandRunResult {
            started_at,
            completed_at: chrono::Utc::now().timestamp(),
            duration_ms: elapsed_ms(started),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            error: None,
        },
        Ok(Err(error)) => {
            command_error_result(started_at, started, error.to_string())
        }
        Err(_) => command_error_result(
            started_at,
            started,
            format!("hook timed out after {}s", handler.timeout_sec),
        ),
    }
}

/// Build a platform default shell command.
fn default_shell_command() -> tokio::process::Command {
    #[cfg(windows)]
    {
        let comspec =
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let mut command = tokio::process::Command::new(comspec);
        command.arg("/C");
        command
    }

    #[cfg(not(windows))]
    {
        let shell =
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut command = tokio::process::Command::new(shell);
        command.arg("-lc");
        command
    }
}

/// Build a failed command result from a runtime error.
fn command_error_result(
    started_at: i64,
    started: Instant,
    error: String,
) -> CommandRunResult {
    CommandRunResult {
        started_at,
        completed_at: chrono::Utc::now().timestamp(),
        duration_ms: elapsed_ms(started),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        error: Some(error),
    }
}

/// Convert elapsed wall-clock time into a bounded millisecond counter.
fn elapsed_ms(started: Instant) -> i64 {
    started.elapsed().as_millis().try_into().unwrap_or(i64::MAX)
}
