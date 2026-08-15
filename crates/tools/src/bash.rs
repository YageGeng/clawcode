use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use protocol::{
    ProductIdentity, SessionId, TruncationDetails, TurnId, UserBashDisposition,
    UserBashResult,
};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::ToolError;
use crate::builtin::output::OutputAccumulator;

const UPDATE_THROTTLE: Duration = Duration::from_millis(100);
const EXIT_STDIO_GRACE: Duration = Duration::from_millis(500);
const OUTPUT_CHANNEL_CAPACITY: usize = 32;

/// Complete parameters for one local shell process shared by tool and user-bash paths.
#[derive(Clone, typed_builder::TypedBuilder)]
pub struct BashExecutionRequest {
    /// Shell source passed to the selected local shell.
    pub command: String,
    /// Existing server-side directory used as the child working directory.
    pub cwd: PathBuf,
    /// Session exposed to the child through the product environment contract.
    pub session_id: SessionId,
    /// Turn exposed to the child through the product environment contract.
    pub turn_id: TurnId,
    /// Cancellation shared with the owning session operation.
    pub cancellation: CancellationToken,
    /// Optional command timeout already validated by the caller.
    #[builder(default, setter(strip_option))]
    pub timeout: Option<Duration>,
}

/// Replaceable visible output emitted while a shared bash execution is running.
#[derive(Debug, Clone, typed_builder::TypedBuilder)]
pub struct BashOutputSnapshot {
    /// Current visible bounded tail.
    pub output: String,
    /// Current truncation accounting.
    pub truncation: TruncationDetails,
    /// Complete-output path once spooling has started.
    #[builder(default)]
    pub full_output_path: Option<PathBuf>,
    /// Bytes in the final source line for partial-line diagnostics.
    pub last_line_bytes: usize,
}

/// Terminal process state retained separately from the serializable user-bash result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashTermination {
    /// The shell exited normally or by a signal.
    Exited,
    /// Session cancellation terminated the process group.
    Aborted,
    /// The caller's validated timeout terminated the process group.
    TimedOut,
}

/// Final shared bash execution value consumed by tool and user-bash adapters.
#[derive(Debug, Clone)]
pub struct BashExecutionOutput {
    /// Pi-compatible command outcome.
    pub result: UserBashResult,
    /// Final bounded output metadata used by tool rendering.
    pub snapshot: BashOutputSnapshot,
    /// Terminal condition used by callers to choose their public error semantics.
    pub termination: BashTermination,
}

/// Executes local shell commands with process-group cancellation and Pi output limits.
#[derive(Debug, Clone, Copy, Default)]
pub struct BashExecutor;

impl BashExecutor {
    /// Runs one command and publishes throttled replaceable output snapshots.
    pub async fn execute(
        request: BashExecutionRequest,
        mut publish: impl FnMut(BashOutputSnapshot) + Send,
    ) -> Result<BashExecutionOutput, ToolError> {
        if request.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if !tokio::fs::try_exists(&request.cwd).await.map_err(|error| {
            ToolError::Execution {
                tool: "bash".to_string(),
                message: error.to_string(),
            }
        })? {
            return Err(ToolError::Execution {
                tool: "bash".to_string(),
                message: format!(
                    "Working directory does not exist: {}\nCannot execute bash commands.",
                    request.cwd.display()
                ),
            });
        }

        let mut command = Command::new(Self::shell_path());
        command
            .arg("-c")
            .arg(&request.command)
            .current_dir(&request.cwd)
            .env(ProductIdentity::SESSION_ID_ENV, request.session_id.as_str())
            .env(ProductIdentity::TURN_ID_ENV, request.turn_id.as_str())
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
        let mut child =
            command.spawn().map_err(|error| ToolError::Execution {
                tool: "bash".to_string(),
                message: error.to_string(),
            })?;
        let child_pid = child.id();
        // A bounded queue keeps fast child output from outrunning spooling and
        // consuming unbounded process memory.
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
        let mut readers = tokio::task::JoinSet::new();
        if let Some(stdout) = child.stdout.take() {
            Self::spawn_reader(&mut readers, stdout, sender.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            Self::spawn_reader(&mut readers, stderr, sender.clone());
        }
        drop(sender);

        let mut output = OutputAccumulator::new();
        let mut last_update = Instant::now()
            .checked_sub(UPDATE_THROTTLE)
            .unwrap_or_else(Instant::now);
        let mut wait = Box::pin(child.wait());
        let mut timeout_wait = Box::pin(async move {
            match request.timeout {
                Some(timeout) => tokio::time::sleep(timeout).await,
                None => std::future::pending::<()>().await,
            }
        });
        let (termination, exit_code) = loop {
            tokio::select! {
                () = request.cancellation.cancelled() => {
                    Self::kill_process_tree(child_pid).await;
                    break (BashTermination::Aborted, None);
                }
                () = &mut timeout_wait => {
                    Self::kill_process_tree(child_pid).await;
                    break (BashTermination::TimedOut, None);
                }
                status = &mut wait => {
                    let status = status.map_err(|error| ToolError::Execution {
                        tool: "bash".to_string(),
                        message: error.to_string(),
                    })?;
                    break (BashTermination::Exited, status.code());
                }
                chunk = receiver.recv() => {
                    let Some(chunk) = chunk else {
                        continue;
                    };
                    output.append(chunk).await.map_err(|error| ToolError::Execution {
                        tool: "bash".to_string(),
                        message: error.to_string(),
                    })?;
                    if last_update.elapsed() >= UPDATE_THROTTLE {
                        publish(Self::snapshot(&output));
                        last_update = Instant::now();
                    }
                }
            }
        };
        if termination != BashTermination::Exited {
            let _status = wait.await;
        }
        let reader_result = tokio::time::timeout(EXIT_STDIO_GRACE, async {
            while readers.join_next().await.is_some() {}
        })
        .await;
        if reader_result.is_err() {
            readers.abort_all();
            while readers.join_next().await.is_some() {}
        }
        while let Some(chunk) = receiver.recv().await {
            output.append(chunk).await.map_err(|error| {
                ToolError::Execution {
                    tool: "bash".to_string(),
                    message: error.to_string(),
                }
            })?;
        }
        let finished =
            output
                .finish()
                .await
                .map_err(|error| ToolError::Execution {
                    tool: "bash".to_string(),
                    message: error.to_string(),
                })?;
        let snapshot = BashOutputSnapshot::builder()
            .output(finished.content)
            .truncation(finished.truncation)
            .full_output_path(finished.full_output_path)
            .last_line_bytes(output.last_line_bytes())
            .build();
        publish(snapshot.clone());
        let result_builder = UserBashResult::builder()
            .disposition(UserBashDisposition::Completed)
            .output(snapshot.output.clone())
            .exit_code(exit_code)
            .cancelled(termination == BashTermination::Aborted)
            .truncated(snapshot.truncation.truncated);
        let result = match &snapshot.full_output_path {
            Some(path) => result_builder
                .full_output_path(path.to_string_lossy().into_owned())
                .build(),
            None => result_builder.build(),
        };
        Ok(BashExecutionOutput {
            result,
            snapshot,
            termination,
        })
    }

    /// Resolves bash using Pi's Unix preference order with a portable sh fallback.
    fn shell_path() -> &'static Path {
        if Path::new("/bin/bash").exists() {
            Path::new("/bin/bash")
        } else {
            Path::new("sh")
        }
    }

    /// Reads one stdout or stderr pipe into the shared arrival-order channel.
    fn spawn_reader(
        readers: &mut tokio::task::JoinSet<()>,
        mut reader: impl AsyncRead + Unpin + Send + 'static,
        sender: tokio::sync::mpsc::Sender<Vec<u8>>,
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

    /// Terminates the command process group so descendants cannot outlive cancellation.
    async fn kill_process_tree(pid: Option<u32>) {
        let Some(pid) = pid else {
            return;
        };
        #[cfg(unix)]
        {
            if let Ok(pid) = i32::try_from(pid) {
                let _result = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(-pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        #[cfg(windows)]
        {
            let _result = Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }

    /// Converts the mutable accumulator into one public replaceable snapshot.
    fn snapshot(output: &OutputAccumulator) -> BashOutputSnapshot {
        let snapshot = output.snapshot();
        BashOutputSnapshot::builder()
            .output(snapshot.content)
            .truncation(snapshot.truncation)
            .full_output_path(snapshot.full_output_path)
            .last_line_bytes(output.last_line_bytes())
            .build()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncWriteExt as _;

    use super::BashExecutor;

    /// A full output channel stops the pipe reader from draining unlimited bytes.
    #[tokio::test]
    async fn reader_applies_backpressure() {
        let (mut writer, reader) = tokio::io::duplex(32 * 1_024);
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let mut readers = tokio::task::JoinSet::new();
        BashExecutor::spawn_reader(&mut readers, reader, sender);
        let writer = tokio::spawn(async move {
            writer
                .write_all(&vec![b'x'; 1024 * 1024])
                .await
                .expect("write source bytes");
        });

        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !writer.is_finished(),
            "reader must apply channel backpressure"
        );

        while receiver.recv().await.is_some() {
            if writer.is_finished() && readers.is_empty() {
                break;
            }
        }
        writer.await.expect("writer task");
        while readers.join_next().await.is_some() {}
    }
}
