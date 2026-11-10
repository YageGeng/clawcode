//! The user's own shell commands (`!cmd`), run to completion with pi's
//! output limits; the model's shell tools live in `exec`.

mod output;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use protocol::{
    ProductIdentity, SessionId, TurnId, UserBashDisposition, UserBashResult,
};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use self::output::OutputAccumulator;
use crate::ToolError;

const UPDATE_THROTTLE: Duration = Duration::from_millis(100);
const EXIT_STDIO_GRACE: Duration = Duration::from_millis(500);
const OUTPUT_CHANNEL_CAPACITY: usize = 32;

/// One user shell command and the Session it runs for.
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
}

/// Executes user shell commands with process-group cancellation and pi's output limits.
#[derive(Debug, Clone, Copy, Default)]
pub struct BashExecutor;

impl BashExecutor {
    /// Runs one command until it exits or is cancelled, publishing its visible
    /// output tail at most every 100 ms and once more at the end.
    pub async fn execute(
        request: BashExecutionRequest,
        mut publish: impl FnMut(String) + Send,
    ) -> Result<UserBashResult, ToolError> {
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
        let (cancelled, exit_code, signal) = loop {
            tokio::select! {
                () = request.cancellation.cancelled() => {
                    Self::kill_process_tree(child_pid).await;
                    break (true, None, Self::killed());
                }
                status = &mut wait => {
                    let status = status.map_err(|error| ToolError::Execution {
                        tool: "bash".to_string(),
                        message: error.to_string(),
                    })?;
                    // Read the signal before the status is dropped; it cannot be recovered later.
                    break (false, status.code(), Self::signal_name(status));
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
                        publish(output.snapshot().content);
                        last_update = Instant::now();
                    }
                }
            }
        };
        if cancelled {
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
        publish(finished.content.clone());
        let result = UserBashResult::builder()
            .disposition(UserBashDisposition::Completed)
            .output(finished.content)
            .exit_code(exit_code)
            .signal(signal)
            .cancelled(cancelled)
            .truncated(finished.truncation.truncated);
        Ok(match finished.full_output_path {
            Some(path) => result
                .full_output_path(path.to_string_lossy().into_owned())
                .build(),
            None => result.build(),
        })
    }

    /// Names the signal that ended the shell, if one did.
    #[cfg(unix)]
    fn signal_name(status: std::process::ExitStatus) -> Option<String> {
        use std::os::unix::process::ExitStatusExt as _;
        status
            .signal()
            .and_then(|signal| nix::sys::signal::Signal::try_from(signal).ok())
            .map(|signal| signal.as_str().to_string())
    }

    /// Other platforms report no signal names.
    #[cfg(not(unix))]
    fn signal_name(_status: std::process::ExitStatus) -> Option<String> {
        None
    }

    /// Signal recorded when we stop the shell: unix sends SIGKILL, Windows uses taskkill.
    fn killed() -> Option<String> {
        cfg!(unix).then(|| "SIGKILL".to_string())
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
