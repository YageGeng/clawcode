//! Terminal backend abstraction used by the shell tool.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use protocol::SessionId;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, mpsc};

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
    /// Whether to attach the command to a pseudo-terminal.
    #[builder(default)]
    pub tty: bool,
    /// Whether stdin should remain writable after process creation.
    #[builder(default)]
    pub stdin_writable: bool,
    /// ACP _meta extension (not used locally).
    #[builder(default, setter(strip_option))]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

impl TerminalCreateParams {
    /// Return environment overrides keyed by variable name.
    fn env_map(&self) -> HashMap<String, String> {
        self.env
            .iter()
            .map(|var| (var.name.clone(), var.value.clone()))
            .collect()
    }
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

impl TerminalOutputSnapshot {
    /// Return whether this snapshot should be treated as a failed tool result.
    pub fn is_error(&self) -> bool {
        // A missing exit status only happens when a forced kill has not reported
        // a concrete wait status yet, so keep it model-visible as a failure.
        self.exit_status
            .as_ref()
            .is_none_or(|exit| exit.exit_code != 0)
    }
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

    /// Write bytes to the command stdin.
    async fn write_stdin(&self, bytes: &[u8]) -> Result<(), TerminalBackendError>;
}

// ── LocalTerminalBackend ──

/// Internal state shared between `LocalRunningTerminal` and the background reader task.
struct LocalTerminalState {
    stdout_bytes: Vec<u8>,
    stderr_bytes: Vec<u8>,
    exited: bool,
    exit_code: i32,
}

/// Process terminator used by local pipe and PTY backends.
trait LocalTerminator: Send + Sync {
    /// Terminate the child process or process group.
    fn terminate(&mut self);
}

/// Unix process-group terminator.
struct ProcessGroupTerminator {
    process_group_id: u32,
}

impl LocalTerminator for ProcessGroupTerminator {
    fn terminate(&mut self) {
        let _ = killpg(Pid::from_raw(self.process_group_id as i32), Signal::SIGKILL);
    }
}

/// Portable-PTY child terminator.
struct PtyTerminator {
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    process_group_id: Option<u32>,
}

impl LocalTerminator for PtyTerminator {
    fn terminate(&mut self) {
        if let Some(process_group_id) = self.process_group_id {
            // Kill the whole process group first so shell-launched descendants
            // do not survive after the PTY child itself is terminated.
            let _ = killpg(Pid::from_raw(process_group_id as i32), Signal::SIGKILL);
        }
        let _ = self.killer.kill();
    }
}

/// Values returned after spawning a local process.
struct LocalSpawn {
    output: LocalSpawnOutput,
    control: LocalSpawnControl,
    keepalive: Option<LocalKeepalive>,
}

/// Output receivers for a spawned local process.
struct LocalSpawnOutput {
    stdout_rx: mpsc::Receiver<Vec<u8>>,
    stderr_rx: mpsc::Receiver<Vec<u8>>,
}

/// Control handles for a spawned local process.
struct LocalSpawnControl {
    writer_tx: mpsc::Sender<Vec<u8>>,
    exit_rx: tokio::sync::oneshot::Receiver<i32>,
    terminator: Box<dyn LocalTerminator>,
}

/// Handles that must remain alive while a PTY child is running.
struct LocalKeepalive {
    _master: Box<dyn portable_pty::MasterPty + Send>,
    _slave: Option<Box<dyn portable_pty::SlavePty + Send>>,
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
        let env = params.env_map();
        let spawned = if params.tty {
            spawn_pty(&params.command, &params.args, &params.cwd, &env).await?
        } else {
            spawn_pipe(
                &params.command,
                &params.args,
                &params.cwd,
                &env,
                params.stdin_writable,
            )
            .await?
        };

        let state = Arc::new(Mutex::new(LocalTerminalState {
            stdout_bytes: Vec::new(),
            stderr_bytes: Vec::new(),
            exited: false,
            exit_code: -1,
        }));
        let exit_notify = Arc::new(Notify::new());
        let exited = Arc::new(AtomicBool::new(false));
        let terminator = Arc::new(Mutex::new(Some(spawned.control.terminator)));

        let reader_state = Arc::clone(&state);
        let reader_notify = Arc::clone(&exit_notify);
        let reader_exited = Arc::clone(&exited);
        let reader_terminator = Arc::clone(&terminator);

        // Spawn a background task that reads both output streams and waits for exit.
        let pipe_state = Arc::clone(&reader_state);
        tokio::spawn(async move {
            let stdout_handle = tokio::spawn(read_channel(
                spawned.output.stdout_rx,
                Arc::clone(&pipe_state),
                false,
            ));
            let stderr_handle =
                tokio::spawn(read_channel(spawned.output.stderr_rx, pipe_state, true));

            let exit_code = spawned.control.exit_rx.await.unwrap_or(-1);
            // Wait for readers to finish so final output is visible with the exit status.
            let _ = tokio::join!(stdout_handle, stderr_handle);

            let mut s = reader_state.lock().await;
            s.exited = true;
            s.exit_code = exit_code;
            drop(s);
            reader_exited.store(true, Ordering::Release);
            // The child has exited naturally, so later Drop must not kill a
            // potentially reused process group id.
            reader_terminator.lock().await.take();
            reader_notify.notify_one();
        });

        Ok(Box::new(LocalRunningTerminal {
            state,
            io: LocalTerminalIo {
                exit_notify,
                writer_tx: spawned.control.writer_tx,
            },
            lifecycle: LocalTerminalLifecycle {
                terminator,
                exited,
                _keepalive: std::sync::Mutex::new(spawned.keepalive),
            },
        }))
    }
}

/// Spawn a non-TTY process with stdout and stderr pipes.
async fn spawn_pipe(
    command: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    stdin_writable: bool,
) -> Result<LocalSpawn, TerminalBackendError> {
    let mut cmd = Command::new(command);

    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if stdin_writable {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    for (name, value) in env {
        cmd.env(name, value);
    }

    #[cfg(unix)]
    {
        // Start a new process group so termination can clean up shell descendants.
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| TerminalBackendError::Io(format!("spawn failed: {e}")))?;
    let process_group_id = child
        .id()
        .ok_or_else(|| TerminalBackendError::Io("spawned process has no pid".to_string()))?;

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| TerminalBackendError::Io("stdout pipe missing".to_string()))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| TerminalBackendError::Io("stderr pipe missing".to_string()))?;

    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let stdin = child.stdin.take();
    tokio::spawn(async move {
        if let Some(mut stdin) = stdin {
            while let Some(bytes) = writer_rx.recv().await {
                let _ = stdin.write_all(&bytes).await;
                let _ = stdin.flush().await;
            }
        }
    });

    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(128);
    tokio::spawn(read_output_stream(BufReader::new(stdout_pipe), stdout_tx));
    tokio::spawn(read_output_stream(BufReader::new(stderr_pipe), stderr_tx));

    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let exit_code = child
            .wait()
            .await
            .ok()
            .and_then(|status| status.code())
            .unwrap_or(-1);
        let _ = exit_tx.send(exit_code);
    });

    Ok(LocalSpawn {
        output: LocalSpawnOutput {
            stdout_rx,
            stderr_rx,
        },
        control: LocalSpawnControl {
            writer_tx,
            exit_rx,
            terminator: Box::new(ProcessGroupTerminator { process_group_id }),
        },
        keepalive: None,
    })
}

/// Spawn a process attached to a local PTY.
async fn spawn_pty(
    command: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<LocalSpawn, TerminalBackendError> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| TerminalBackendError::Io(format!("openpty failed: {e}")))?;

    let mut builder = CommandBuilder::new(command);
    builder.cwd(cwd);
    for arg in args {
        builder.arg(arg);
    }
    for (name, value) in env {
        builder.env(name, value);
    }

    let mut child = pair
        .slave
        .spawn_command(builder)
        .map_err(|e| TerminalBackendError::Io(format!("pty spawn failed: {e}")))?;
    let process_group_id = child.process_id();
    let killer = child.clone_killer();

    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| TerminalBackendError::Io(format!("pty writer failed: {e}")))?;
    tokio::spawn(async move {
        while let Some(bytes) = writer_rx.recv().await {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    });

    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (_stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(1);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| TerminalBackendError::Io(format!("pty reader failed: {e}")))?;
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Some(chunk) = buf.get(..n) {
                        let _ = stdout_tx.blocking_send(chunk.to_vec());
                    }
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });

    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let exit_code = child.wait().map_or(-1, |status| status.exit_code() as i32);
        let _ = exit_tx.send(exit_code);
    });

    Ok(LocalSpawn {
        output: LocalSpawnOutput {
            stdout_rx,
            stderr_rx,
        },
        control: LocalSpawnControl {
            writer_tx,
            exit_rx,
            terminator: Box::new(PtyTerminator {
                killer,
                process_group_id,
            }),
        },
        keepalive: Some(LocalKeepalive {
            _master: pair.master,
            _slave: None,
        }),
    })
}

/// Read from an async output stream and forward byte chunks.
async fn read_output_stream<R>(mut reader: R, output_tx: mpsc::Sender<Vec<u8>>)
where
    R: AsyncRead + Unpin,
{
    let mut buf = vec![0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if let Some(chunk) = buf.get(..n) {
                    let _ = output_tx.send(chunk.to_vec()).await;
                }
            }
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// Read from an output channel and append to shared state.
async fn read_channel(
    mut reader: mpsc::Receiver<Vec<u8>>,
    state: Arc<Mutex<LocalTerminalState>>,
    is_stderr: bool,
) {
    while let Some(chunk) = reader.recv().await {
        let mut s = state.lock().await;
        if is_stderr {
            s.stderr_bytes.extend_from_slice(&chunk);
        } else {
            s.stdout_bytes.extend_from_slice(&chunk);
        }
    }
}

/// Local handle representing a running process.
struct LocalRunningTerminal {
    state: Arc<Mutex<LocalTerminalState>>,
    io: LocalTerminalIo,
    lifecycle: LocalTerminalLifecycle,
}

/// I/O handles for a running local terminal.
struct LocalTerminalIo {
    exit_notify: Arc<Notify>,
    writer_tx: mpsc::Sender<Vec<u8>>,
}

/// Lifecycle handles for a running local terminal.
struct LocalTerminalLifecycle {
    terminator: Arc<Mutex<Option<Box<dyn LocalTerminator>>>>,
    exited: Arc<AtomicBool>,
    _keepalive: std::sync::Mutex<Option<LocalKeepalive>>,
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
            self.io.exit_notify.notified().await;
        }
    }

    async fn kill(&self) -> Result<(), TerminalBackendError> {
        if let Some(mut terminator) = self.lifecycle.terminator.lock().await.take() {
            terminator.terminate();
        }
        let mut s = self.state.lock().await;
        if !s.exited {
            s.exited = true;
            s.exit_code = -1;
        }
        self.lifecycle.exited.store(true, Ordering::Release);
        self.io.exit_notify.notify_one();
        Ok(())
    }

    async fn write_stdin(&self, bytes: &[u8]) -> Result<(), TerminalBackendError> {
        if self.lifecycle.exited.load(Ordering::Acquire) {
            return Err(TerminalBackendError::InvalidRequest(
                "process has already exited".to_string(),
            ));
        }
        self.io
            .writer_tx
            .send(bytes.to_vec())
            .await
            .map_err(|error| TerminalBackendError::Io(format!("stdin is closed: {error}")))
    }
}

impl Drop for LocalRunningTerminal {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.lifecycle.terminator.try_lock()
            && let Some(mut terminator) = guard.take()
        {
            terminator.terminate();
        }
    }
}
