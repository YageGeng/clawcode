use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use super::{SpawnedTerminal, TerminalBackend, TerminalExit, shell_command};
use crate::{TerminalError, TerminalSpawnRequest};

const OUTPUT_CHANNEL_CAPACITY: usize = 32;
const EXIT_STDIO_GRACE: Duration = Duration::from_millis(500);

/// Process control request serialized through the child-owning supervisor.
enum PipeControlRequest {
    /// Sends an interrupt to the process tree.
    Interrupt(oneshot::Sender<Result<(), TerminalError>>),
    /// Forcefully terminates the process tree.
    Terminate(oneshot::Sender<Result<(), TerminalError>>),
}

/// Cloneable control handle for one pipe process.
#[derive(Debug)]
struct PipeControl {
    sender: mpsc::UnboundedSender<PipeControlRequest>,
}

impl Drop for PipeControl {
    /// Requests best-effort tree termination when the final control owner disappears.
    fn drop(&mut self) {
        let (acknowledge, _receiver) = oneshot::channel();
        // The child-owning supervisor remains alive until it consumes this
        // request, so dropping manager ownership cannot silently orphan it.
        let _result =
            self.sender.send(PipeControlRequest::Terminate(acknowledge));
    }
}

#[async_trait]
impl TerminalBackend for PipeControl {
    /// Rejects ordinary pipe input while accepting Ctrl-C as an interrupt.
    async fn write(&self, chars: &str) -> Result<(), TerminalError> {
        if chars == "\u{3}" {
            self.interrupt().await
        } else {
            Err(TerminalError::Backend {
                message: "pipe backend does not accept ordinary stdin"
                    .to_string(),
            })
        }
    }

    /// Sends one serialized interrupt request to the child supervisor.
    async fn interrupt(&self) -> Result<(), TerminalError> {
        self.request(false).await
    }

    /// Sends one serialized termination request to the child supervisor.
    async fn terminate(&self) -> Result<(), TerminalError> {
        self.request(true).await
    }
}

impl PipeControl {
    /// Sends one control request and waits for the supervisor acknowledgement.
    async fn request(&self, terminate: bool) -> Result<(), TerminalError> {
        let (sender, receiver) = oneshot::channel();
        let request = if terminate {
            PipeControlRequest::Terminate(sender)
        } else {
            PipeControlRequest::Interrupt(sender)
        };
        self.sender
            .send(request)
            .map_err(|_error| TerminalError::Backend {
                message: "terminal process is no longer running".to_string(),
            })?;
        receiver.await.map_err(|_error| TerminalError::Backend {
            message: "terminal process control acknowledgement was dropped"
                .to_string(),
        })?
    }
}

/// Spawns one shell command with piped output and process-tree ownership.
pub(super) async fn spawn_pipe(
    request: TerminalSpawnRequest,
) -> Result<SpawnedTerminal, TerminalError> {
    let (shell, argument) = shell_command();
    let mut command = Command::new(shell);
    command
        .arg(argument)
        .arg(&request.cmd)
        .current_dir(&request.cwd)
        .env(
            protocol::ProductIdentity::SESSION_ID_ENV,
            request.session_id.as_str(),
        )
        .env(
            protocol::ProductIdentity::TURN_ID_ENV,
            request.turn_id.as_str(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        std::os::unix::process::CommandExt::process_group(
            command.as_std_mut(),
            0,
        );
    }
    let mut child = command.spawn().map_err(|error| TerminalError::Spawn {
        message: error.to_string(),
    })?;
    let os_pid = child.id();

    #[cfg(windows)]
    let job = assign_windows_job(&child)?;

    let (output_sender, output_receiver) =
        mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
    let mut readers = tokio::task::JoinSet::new();
    if let Some(stdout) = child.stdout.take() {
        spawn_reader(&mut readers, stdout, output_sender.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(&mut readers, stderr, output_sender.clone());
    }
    drop(output_sender);

    let (control_sender, mut control_receiver) = mpsc::unbounded_channel();
    let (exit_sender, exit_receiver) = oneshot::channel();
    tokio::spawn(async move {
        let mut control_open = true;
        let exit = loop {
            tokio::select! {
                status = child.wait() => {
                    break match status {
                        Ok(status) => TerminalExit {
                            exit_code: status.code(),
                            failure: None,
                        },
                        Err(error) => TerminalExit {
                            exit_code: None,
                            failure: Some(error.to_string()),
                        },
                    };
                }
                request = control_receiver.recv(), if control_open => {
                    let Some(request) = request else {
                        control_open = false;
                        continue;
                    };
                    match request {
                        PipeControlRequest::Interrupt(acknowledge) => {
                            let result = interrupt_process_tree(os_pid).await;
                            let _result = acknowledge.send(result);
                        }
                        PipeControlRequest::Terminate(acknowledge) => {
                            #[cfg(windows)]
                            let result = terminate_windows_job(&job, os_pid).await;
                            #[cfg(not(windows))]
                            let result = terminate_process_tree(os_pid).await;
                            let _result = acknowledge.send(result);
                        }
                    }
                }
            }
        };
        let settled = tokio::time::timeout(EXIT_STDIO_GRACE, async {
            while readers.join_next().await.is_some() {}
        })
        .await;
        if settled.is_err() {
            readers.abort_all();
            while readers.join_next().await.is_some() {}
        }
        let _result = exit_sender.send(exit);
    });

    Ok(SpawnedTerminal::builder()
        .control(Arc::new(PipeControl {
            sender: control_sender,
        }))
        .output(output_receiver)
        .exit(exit_receiver)
        .os_pid(os_pid)
        .build())
}

/// Continuously drains one stdout or stderr stream into the shared output queue.
fn spawn_reader(
    readers: &mut tokio::task::JoinSet<()>,
    mut reader: impl AsyncRead + Unpin + Send + 'static,
    sender: mpsc::Sender<Vec<u8>>,
) {
    readers.spawn(async move {
        let mut buffer = vec![0_u8; 8 * 1_024];
        loop {
            let read = match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let Some(chunk) = buffer.get(..read) else {
                break;
            };
            if sender.send(chunk.to_vec()).await.is_err() {
                break;
            }
        }
    });
}

/// Sends an interrupt to the complete process tree.
async fn interrupt_process_tree(pid: Option<u32>) -> Result<(), TerminalError> {
    #[cfg(unix)]
    {
        signal_process_group(pid, nix::sys::signal::Signal::SIGINT)
    }
    #[cfg(windows)]
    {
        taskkill_process_tree(pid, false).await
    }
}

/// Forcefully terminates the complete process tree.
async fn terminate_process_tree(pid: Option<u32>) -> Result<(), TerminalError> {
    #[cfg(unix)]
    {
        signal_process_group(pid, nix::sys::signal::Signal::SIGKILL)
    }
    #[cfg(windows)]
    {
        taskkill_process_tree(pid, true).await
    }
}

#[cfg(unix)]
/// Sends one Unix signal to the process group led by the child PID.
fn signal_process_group(
    pid: Option<u32>,
    signal: nix::sys::signal::Signal,
) -> Result<(), TerminalError> {
    let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) else {
        return Ok(());
    };
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pid), signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(error) => Err(TerminalError::Backend {
            message: error.to_string(),
        }),
    }
}

#[cfg(windows)]
/// Assigns one child to a kill-on-close Windows Job Object.
fn assign_windows_job(
    child: &tokio::process::Child,
) -> Result<win32job::Job, TerminalError> {
    use std::os::windows::io::AsRawHandle as _;

    let mut limits = win32job::ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job =
        win32job::Job::create_with_limit_info(&limits).map_err(|error| {
            TerminalError::Spawn {
                message: error.to_string(),
            }
        })?;
    job.assign_process(child.as_raw_handle() as isize)
        .map_err(|error| TerminalError::Spawn {
            message: error.to_string(),
        })?;
    Ok(job)
}

#[cfg(windows)]
/// Uses the Job lifetime and taskkill fallback to terminate descendants.
async fn terminate_windows_job(
    job: &win32job::Job,
    pid: Option<u32>,
) -> Result<(), TerminalError> {
    let _job_handle = job.handle();
    taskkill_process_tree(pid, true).await
}

#[cfg(windows)]
/// Invokes the platform process-tree utility for one manager-owned PID.
async fn taskkill_process_tree(
    pid: Option<u32>,
    force: bool,
) -> Result<(), TerminalError> {
    let Some(pid) = pid else {
        return Ok(());
    };
    let pid = pid.to_string();
    let mut command = Command::new("taskkill");
    command.args(["/T", "/PID", &pid]);
    if force {
        command.arg("/F");
    }
    let status =
        command
            .status()
            .await
            .map_err(|error| TerminalError::Backend {
                message: error.to_string(),
            })?;
    if status.success() {
        Ok(())
    } else {
        Err(TerminalError::Backend {
            message: format!("taskkill exited with status {status}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use protocol::{SessionId, TurnId};
    use tokio_util::sync::CancellationToken;

    use super::{SpawnedTerminal, TerminalExit, spawn_pipe};
    use crate::TerminalSpawnRequest;

    /// Complete test-only output and status from one pipe process.
    struct CapturedPipe {
        output: String,
        exit_code: Option<i32>,
    }

    /// Builds one deterministic pipe request in the current directory.
    fn test_request(command: &str) -> TerminalSpawnRequest {
        TerminalSpawnRequest::builder()
            .cmd(command.to_string())
            .cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .session_id(
                SessionId::try_from("session-pipe").expect("session id"),
            )
            .turn_id(TurnId::try_from("turn-pipe").expect("turn id"))
            .tty(false)
            .yield_duration(Duration::from_secs(1))
            .max_output_tokens(10_000)
            .cancellation(CancellationToken::new())
            .build()
    }

    /// Drains every output chunk before returning the supervisor exit status.
    async fn collect_until_exit(
        mut spawned: SpawnedTerminal,
    ) -> Result<CapturedPipe, String> {
        let mut output = Vec::new();
        while let Some(chunk) = spawned.output.recv().await {
            output.extend_from_slice(&chunk);
        }
        let TerminalExit { exit_code, failure } =
            spawned.exit.await.map_err(|error| error.to_string())?;
        if let Some(failure) = failure {
            return Err(failure);
        }
        Ok(CapturedPipe {
            output: String::from_utf8_lossy(&output).into_owned(),
            exit_code,
        })
    }

    /// Pipe output preserves observed stdout/stderr arrival order and exit status.
    #[tokio::test]
    async fn pipe_backend_merges_stdout_stderr_and_reports_exit() {
        let spawned = spawn_pipe(test_request(
            "printf out; sleep 0.05; printf err >&2; exit 7",
        ))
        .await
        .expect("spawn pipe");
        let captured = collect_until_exit(spawned).await.expect("collect pipe");
        assert_eq!(captured.output, "outerr");
        assert_eq!(captured.exit_code, Some(7));
    }

    #[cfg(unix)]
    /// Terminating one pipe backend prevents descendants from outliving it.
    #[tokio::test]
    async fn pipe_termination_kills_descendant_process_group() {
        let marker = tempfile::NamedTempFile::new().expect("marker");
        let spawned = spawn_pipe(test_request(&format!(
            "(sleep 2; printf leaked > {}) & sleep 30",
            marker.path().display()
        )))
        .await
        .expect("spawn process tree");
        spawned
            .control
            .terminate()
            .await
            .expect("terminate process tree");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(
            std::fs::metadata(marker.path())
                .expect("marker metadata")
                .len(),
            0
        );
    }

    /// Dropping the last pipe control owner still reaps the managed process tree.
    #[tokio::test]
    async fn terminal_drop_reaps_pipe_process() {
        let spawned = spawn_pipe(test_request("sleep 30"))
            .await
            .expect("spawn pipe sleeper");
        drop(spawned.control);
        let exit = tokio::time::timeout(Duration::from_secs(3), spawned.exit)
            .await
            .expect("pipe exits after control drop")
            .expect("pipe exit channel");
        assert_ne!(exit.exit_code, Some(0));
    }
}
