//! Terminal backend abstraction used by the shell tool.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use protocol::SessionId;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};

/// Error returned by terminal backend implementations.
#[derive(Debug, Error)]
pub enum TerminalBackendError {
    /// The request was invalid for the backend.
    #[error("{0}")]
    InvalidRequest(String),
    /// A terminal or transport operation failed.
    #[error("{0}")]
    Io(String),
}

/// Parameters for creating a terminal.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct TerminalCreateParams {
    /// Session id used by session-scoped backends.
    pub session_id: SessionId,
    /// The executable to run (e.g. "/bin/sh").
    pub command: String,
    /// Arguments to the command (e.g. ["-c", "user command"]).
    pub args: Vec<String>,
    /// Environment variables for the command.
    #[builder(default)]
    pub env: Vec<TerminalEnvVariable>,
    /// Working directory for the command.
    pub cwd: PathBuf,
    /// Maximum number of output bytes to retain (not enforced locally).
    #[builder(default, setter(strip_option))]
    pub output_byte_limit: Option<u64>,
    /// ACP _meta extension (not used locally).
    #[builder(default, setter(strip_option))]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Environment variable key-value pair.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct TerminalEnvVariable {
    /// Variable name.
    pub name: String,
    /// Variable value.
    pub value: String,
}

/// A snapshot of the terminal output at a point in time.
#[derive(Debug, Clone, Default)]
pub struct TerminalOutputSnapshot {
    /// Full stdout output since creation.
    pub stdout: String,
    /// Full stderr output since creation (empty for ACP backend which combines streams).
    pub stderr: String,
    /// Exit status, if the process has exited.
    pub exit_status: Option<TerminalExitResult>,
}

/// Exit result of a completed terminal command.
#[derive(Debug, Clone)]
pub struct TerminalExitResult {
    /// Exit code from the process.
    pub exit_code: i32,
}

/// Backend abstraction for running shell/terminal commands.
///
/// Implementations may execute locally (via OS processes) or delegate
/// to a remote ACP client.
#[async_trait]
pub trait TerminalBackend: Send + Sync {
    /// Start a command and return a running terminal handle.
    ///
    /// The returned handle must be held until the caller is finished
    /// with the terminal; dropping it will release resources.
    async fn create(
        &self,
        params: TerminalCreateParams,
    ) -> Result<Box<dyn RunningTerminal>, TerminalBackendError>;
}

/// Handle to a running terminal, providing output polling and lifecycle control.
///
/// Implementations are expected to clean up the underlying process on drop.
#[async_trait]
pub trait RunningTerminal: Send + Sync {
    /// Non-blocking snapshot of current output and exit status.
    ///
    /// Returns accumulated stdout/stderr since creation. If the process
    /// has already exited, `exit_status` will be `Some`.
    async fn output(&self) -> Result<TerminalOutputSnapshot, TerminalBackendError>;

    /// Block until the command exits, returning the exit result.
    async fn wait_for_exit(&self) -> Result<TerminalExitResult, TerminalBackendError>;

    /// Kill the running command.
    ///
    /// After killing, [`output`] will reflect that the process has exited
    /// (with exit code -1) and [`wait_for_exit`] will return immediately.
    async fn kill(&self) -> Result<(), TerminalBackendError>;
}

// ── LocalTerminalBackend ──

/// Internal state shared between `LocalRunningTerminal` and the background reader task.
struct LocalTerminalState {
    stdout_bytes: Vec<u8>,
    stderr_bytes: Vec<u8>,
    exited: bool,
    exit_code: i32,
}

/// Local terminal backend that spawns OS processes via tokio.
///
/// Creates a child process with piped stdout/stderr. Background tasks
/// read from both pipes and append to shared buffers. Exit is signalled
/// via a [`Notify`].
#[derive(Debug, Default)]
pub struct LocalTerminalBackend;

impl LocalTerminalBackend {
    /// Create a local terminal backend.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TerminalBackend for LocalTerminalBackend {
    async fn create(
        &self,
        params: TerminalCreateParams,
    ) -> Result<Box<dyn RunningTerminal>, TerminalBackendError> {
        let mut cmd = Command::new(&params.command);
        cmd.args(&params.args)
            .current_dir(&params.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for env_var in &params.env {
            cmd.env(&env_var.name, &env_var.value);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| TerminalBackendError::Io(format!("spawn failed: {e}")))?;

        let stdout_pipe = child.stdout.take().expect("stdout pipe configured");
        let stderr_pipe = child.stderr.take().expect("stderr pipe configured");

        let state = Arc::new(Mutex::new(LocalTerminalState {
            stdout_bytes: Vec::new(),
            stderr_bytes: Vec::new(),
            exited: false,
            exit_code: -1,
        }));
        let exit_notify = Arc::new(Notify::new());

        let reader_state = Arc::clone(&state);
        let reader_notify = Arc::clone(&exit_notify);

        // Spawn a background task that reads both pipes and waits for exit.
        let pipe_state = Arc::clone(&reader_state);
        tokio::spawn(async move {
            let stdout_handle =
                tokio::spawn(read_pipe(stdout_pipe, Arc::clone(&pipe_state), false));
            let stderr_handle = tokio::spawn(read_pipe(stderr_pipe, pipe_state, true));

            let status = child.wait().await;
            // Wait for pipe readers to finish.
            let _ = tokio::join!(stdout_handle, stderr_handle);

            let exit_code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
            let mut s = reader_state.lock().await;
            s.exited = true;
            s.exit_code = exit_code;
            drop(s);
            reader_notify.notify_one();
        });

        Ok(Box::new(LocalRunningTerminal { state, exit_notify }))
    }
}

/// Read from a child process pipe and append to shared state.
async fn read_pipe<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    state: Arc<Mutex<LocalTerminalState>>,
    is_stderr: bool,
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let mut s = state.lock().await;
                let chunk = buf.get(..n).expect("read byte count within buffer bounds");
                if is_stderr {
                    s.stderr_bytes.extend_from_slice(chunk);
                } else {
                    s.stdout_bytes.extend_from_slice(chunk);
                }
            }
            Err(_) => break,
        }
    }
}

/// Local handle representing a running process.
struct LocalRunningTerminal {
    state: Arc<Mutex<LocalTerminalState>>,
    exit_notify: Arc<Notify>,
}

#[async_trait]
impl RunningTerminal for LocalRunningTerminal {
    async fn output(&self) -> Result<TerminalOutputSnapshot, TerminalBackendError> {
        let s = self.state.lock().await;
        Ok(TerminalOutputSnapshot {
            stdout: String::from_utf8_lossy(&s.stdout_bytes).to_string(),
            stderr: String::from_utf8_lossy(&s.stderr_bytes).to_string(),
            exit_status: s.exited.then(|| TerminalExitResult {
                exit_code: s.exit_code,
            }),
        })
    }

    async fn wait_for_exit(&self) -> Result<TerminalExitResult, TerminalBackendError> {
        loop {
            {
                let s = self.state.lock().await;
                if s.exited {
                    return Ok(TerminalExitResult {
                        exit_code: s.exit_code,
                    });
                }
            }
            self.exit_notify.notified().await;
        }
    }

    async fn kill(&self) -> Result<(), TerminalBackendError> {
        let mut s = self.state.lock().await;
        if !s.exited {
            s.exited = true;
            s.exit_code = -1;
        }
        self.exit_notify.notify_one();
        Ok(())
    }
}
