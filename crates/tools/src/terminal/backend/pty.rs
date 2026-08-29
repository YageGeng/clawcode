use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::{mpsc, oneshot};

use super::{SpawnedTerminal, TerminalBackend, TerminalExit, shell_command};
use crate::{TerminalError, TerminalSpawnRequest};

const OUTPUT_CHANNEL_CAPACITY: usize = 32;
const EXIT_STDIO_GRACE: Duration = Duration::from_millis(500);

/// Platform-owned process-tree resource retained for forceful cleanup.
enum PlatformProcessTree {
    #[cfg(unix)]
    Unix(Option<i32>),
    #[cfg(windows)]
    Windows(win32job::Job),
}

/// Mutable termination resources shared with the asynchronous control handle.
struct PtyTerminator {
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    process_tree: PlatformProcessTree,
    completion: Arc<Mutex<bool>>,
}

impl Drop for PtyTerminator {
    /// Performs best-effort tree cleanup when the final PTY owner disappears.
    fn drop(&mut self) {
        if let Err(error) = terminate_process_tree(self) {
            tracing::warn!(
                "failed to terminate PTY process tree on drop: {}",
                error
            );
        }
    }
}

/// Cloneable control handle for one native pseudo-terminal.
struct PtyControl {
    writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    terminator: Arc<Mutex<PtyTerminator>>,
}

#[async_trait]
impl TerminalBackend for PtyControl {
    /// Writes and flushes interactive input without blocking a Tokio worker.
    async fn write(&self, chars: &str) -> Result<(), TerminalError> {
        let writer = Arc::clone(&self.writer);
        let bytes = chars.as_bytes().to_vec();
        tokio::task::spawn_blocking(move || {
            let mut writer = writer
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            writer.write_all(&bytes).map_err(|error| {
                TerminalError::Backend {
                    message: error.to_string(),
                }
            })?;
            writer.flush().map_err(|error| TerminalError::Backend {
                message: error.to_string(),
            })
        })
        .await
        .map_err(|error| TerminalError::Backend {
            message: error.to_string(),
        })?
    }

    /// Delivers Ctrl-C through the PTY line discipline to its foreground group.
    async fn interrupt(&self) -> Result<(), TerminalError> {
        self.write("\u{3}").await
    }

    /// Forcefully terminates the complete PTY process tree off the async worker.
    async fn terminate(&self) -> Result<(), TerminalError> {
        let terminator = Arc::clone(&self.terminator);
        tokio::task::spawn_blocking(move || {
            let mut terminator = terminator
                .lock()
                .map_err(|_error| TerminalError::StatePoisoned)?;
            terminate_process_tree(&mut terminator)
        })
        .await
        .map_err(|error| TerminalError::Backend {
            message: error.to_string(),
        })?
    }
}

/// Spawns one shell command inside a native pseudo-terminal.
pub(super) async fn spawn_pty(
    request: TerminalSpawnRequest,
) -> Result<SpawnedTerminal, TerminalError> {
    let resources = tokio::task::spawn_blocking(move || {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| TerminalError::Spawn {
                message: error.to_string(),
            })?;
        let (shell, argument) = shell_command();
        let mut command = CommandBuilder::new(shell);
        command.arg(argument);
        command.arg(&request.cmd);
        command.cwd(&request.cwd);
        command.env(
            protocol::ProductIdentity::SESSION_ID_ENV,
            request.session_id.as_str(),
        );
        command.env(
            protocol::ProductIdentity::TURN_ID_ENV,
            request.turn_id.as_str(),
        );
        let child = pair.slave.spawn_command(command).map_err(|error| {
            TerminalError::Spawn {
                message: error.to_string(),
            }
        })?;
        let os_pid = child.process_id();
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().map_err(|error| {
            TerminalError::Spawn {
                message: error.to_string(),
            }
        })?;
        let writer = pair.master.take_writer().map_err(|error| {
            TerminalError::Spawn {
                message: error.to_string(),
            }
        })?;
        #[cfg(unix)]
        let process_tree = PlatformProcessTree::Unix(
            pair.master
                .process_group_leader()
                .or_else(|| os_pid.and_then(|pid| i32::try_from(pid).ok())),
        );
        #[cfg(windows)]
        let process_tree =
            PlatformProcessTree::Windows(assign_windows_job(&*child)?);
        drop(pair.slave);
        Ok::<_, TerminalError>((
            child,
            pair.master,
            reader,
            writer,
            killer,
            process_tree,
            os_pid,
        ))
    })
    .await
    .map_err(|error| TerminalError::Spawn {
        message: error.to_string(),
    })??;
    let (mut child, master, mut reader, writer, killer, process_tree, os_pid) =
        resources;
    let completion = Arc::new(Mutex::new(false));
    let terminator = Arc::new(Mutex::new(PtyTerminator {
        killer,
        process_tree,
        completion: Arc::clone(&completion),
    }));

    let (output_sender, output_receiver) =
        mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
    let mut reader_task = tokio::task::spawn_blocking(move || {
        let mut buffer = vec![0_u8; 8 * 1_024];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let Some(chunk) = buffer.get(..read) else {
                break;
            };
            if output_sender.blocking_send(chunk.to_vec()).is_err() {
                break;
            }
        }
    });
    let wait_completion = Arc::clone(&completion);
    let wait_task = tokio::task::spawn_blocking(move || {
        let status = child.wait();
        if status.is_ok() {
            // Disarm under the same lock used by explicit termination. Either
            // termination wins while the original process still exists, or
            // completion wins before a stale process-group id can be signaled.
            match wait_completion.lock() {
                Ok(mut completed) => *completed = true,
                Err(_error) => {
                    tracing::warn!(
                        "failed to disarm completed PTY termination state"
                    );
                }
            }
        }
        // Keep the master alive until wait completes; some PTY implementations
        // treat an early master close as an implicit hangup.
        drop(master);
        status
    });
    let (exit_sender, exit_receiver) = oneshot::channel();
    tokio::spawn(async move {
        let exit = match wait_task.await {
            Ok(Ok(status)) => TerminalExit {
                exit_code: i32::try_from(status.exit_code()).ok(),
                failure: None,
            },
            Ok(Err(error)) => TerminalExit {
                exit_code: None,
                failure: Some(error.to_string()),
            },
            Err(error) => TerminalExit {
                exit_code: None,
                failure: Some(error.to_string()),
            },
        };
        if tokio::time::timeout(EXIT_STDIO_GRACE, &mut reader_task)
            .await
            .is_err()
        {
            reader_task.abort();
        }
        let _result = exit_sender.send(exit);
    });

    Ok(SpawnedTerminal::builder()
        .control(Arc::new(PtyControl {
            writer: Arc::new(Mutex::new(writer)),
            terminator,
        }))
        .output(output_receiver)
        .exit(exit_receiver)
        .os_pid(os_pid)
        .build())
}

/// Terminates one PTY tree and tolerates already-exited process handles.
fn terminate_process_tree(
    terminator: &mut PtyTerminator,
) -> Result<(), TerminalError> {
    // Hold completion through signal delivery so child reaping and process-tree
    // termination have one linearization point even if identifiers are reused.
    let completion = Arc::clone(&terminator.completion);
    let completed = completion
        .lock()
        .map_err(|_error| TerminalError::StatePoisoned)?;
    if *completed {
        return Ok(());
    }
    #[cfg(unix)]
    let tree_result = match &terminator.process_tree {
        PlatformProcessTree::Unix(Some(process_group)) => {
            match nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-*process_group),
                nix::sys::signal::Signal::SIGKILL,
            ) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
                Err(error) => Err(TerminalError::Backend {
                    message: error.to_string(),
                }),
            }
        }
        PlatformProcessTree::Unix(None) => Ok(()),
    };
    #[cfg(windows)]
    let tree_result = match &terminator.process_tree {
        PlatformProcessTree::Windows(job) => {
            job.terminate(1).map_err(|error| TerminalError::Backend {
                message: error.to_string(),
            })
        }
    };
    let child_result = terminator.killer.kill();
    tree_result?;
    match child_result {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(3) =>
        {
            Ok(())
        }
        Err(error) => Err(TerminalError::Backend {
            message: error.to_string(),
        }),
    }
}

#[cfg(windows)]
/// Assigns one portable PTY child to a kill-on-close Windows Job Object.
fn assign_windows_job(
    child: &dyn portable_pty::Child,
) -> Result<win32job::Job, TerminalError> {
    let mut limits = win32job::ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job =
        win32job::Job::create_with_limit_info(&limits).map_err(|error| {
            TerminalError::Spawn {
                message: error.to_string(),
            }
        })?;
    let handle = child.as_raw_handle().ok_or_else(|| TerminalError::Spawn {
        message: "PTY child did not expose a Windows process handle"
            .to_string(),
    })?;
    job.assign_process(handle as isize).map_err(|error| {
        TerminalError::Spawn {
            message: error.to_string(),
        }
    })?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use protocol::{SessionId, TurnId};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::{PlatformProcessTree, PtyTerminator, spawn_pty};
    use crate::TerminalSpawnRequest;
    use crate::terminal::backend::{SpawnedTerminal, TerminalExit};

    /// Complete test-only output and status from one PTY process.
    struct CapturedPty {
        output: String,
        exit_code: Option<i32>,
    }

    /// Counts kill requests issued by one test terminator.
    #[derive(Debug)]
    struct CountingKiller(Arc<AtomicUsize>);

    impl portable_pty::ChildKiller for CountingKiller {
        /// Records one kill request without signaling a real process.
        fn kill(&mut self) -> std::io::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        /// Clones the counter into one independently owned killer handle.
        fn clone_killer(
            &self,
        ) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(Self(Arc::clone(&self.0)))
        }
    }

    /// Builds one deterministic native PTY request in the current directory.
    fn test_request(command: &str) -> TerminalSpawnRequest {
        TerminalSpawnRequest::builder()
            .cmd(command.to_string())
            .cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .session_id(SessionId::try_from("session-pty").expect("session id"))
            .turn_id(TurnId::try_from("turn-pty").expect("turn id"))
            .tty(true)
            .yield_duration(Duration::from_secs(1))
            .max_output_tokens(10_000)
            .cancellation(CancellationToken::new())
            .build()
    }

    /// Drains PTY output until a required interactive prompt is observed.
    async fn wait_for_output(
        output: &mut mpsc::Receiver<Vec<u8>>,
        captured: &mut Vec<u8>,
        expected: &str,
    ) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !String::from_utf8_lossy(captured).contains(expected) {
                let chunk =
                    output.recv().await.expect("PTY output before prompt");
                captured.extend_from_slice(&chunk);
            }
        })
        .await
        .expect("PTY prompt timeout");
    }

    /// Drains remaining PTY output before returning its final exit status.
    async fn collect_until_exit(
        mut output: mpsc::Receiver<Vec<u8>>,
        exit: tokio::sync::oneshot::Receiver<TerminalExit>,
        mut captured: Vec<u8>,
    ) -> Result<CapturedPty, String> {
        while let Some(chunk) = output.recv().await {
            captured.extend_from_slice(&chunk);
        }
        let TerminalExit { exit_code, failure } =
            exit.await.map_err(|error| error.to_string())?;
        if let Some(failure) = failure {
            return Err(failure);
        }
        Ok(CapturedPty {
            output: String::from_utf8_lossy(&captured).into_owned(),
            exit_code,
        })
    }

    /// A native PTY exposes terminal descriptors and carries interactive input.
    #[tokio::test]
    async fn pty_backend_exposes_a_real_terminal_and_accepts_input() {
        let SpawnedTerminal {
            control,
            mut output,
            exit,
            ..
        } = spawn_pty(test_request(
            "python3 -u -c 'import os; print(os.isatty(0), os.isatty(1)); print(input(\"name: \"))'",
        ))
        .await
        .expect("spawn real pty");
        let mut captured = Vec::new();
        wait_for_output(&mut output, &mut captured, "name: ").await;
        control.write("clawcode\n").await.expect("write PTY");
        let captured = collect_until_exit(output, exit, captured)
            .await
            .expect("collect PTY");
        assert!(captured.output.contains("True True"));
        assert!(captured.output.contains("clawcode"));
        assert_eq!(captured.exit_code, Some(0));
    }

    #[cfg(unix)]
    /// Ctrl-C interrupts the PTY foreground process group within a bounded wait.
    #[tokio::test]
    async fn pty_ctrl_c_interrupts_foreground_process_group() {
        let spawned = spawn_pty(test_request("sleep 30"))
            .await
            .expect("spawn PTY sleeper");
        spawned.control.write("\u{3}").await.expect("send Ctrl-C");
        let exit = tokio::time::timeout(Duration::from_secs(3), spawned.exit)
            .await
            .expect("PTY exits after Ctrl-C")
            .expect("exit channel");
        assert_ne!(exit.exit_code, Some(0));
    }

    /// Dropping the last PTY control owner still reaps the managed process tree.
    #[tokio::test]
    async fn terminal_drop_reaps_pty_process() {
        let spawned = spawn_pty(test_request("sleep 30"))
            .await
            .expect("spawn PTY sleeper");
        drop(spawned.control);
        let exit = tokio::time::timeout(Duration::from_secs(3), spawned.exit)
            .await
            .expect("PTY exits after control drop")
            .expect("PTY exit channel");
        assert_ne!(exit.exit_code, Some(0));
    }

    #[cfg(unix)]
    /// A naturally completed PTY never signals its stale process-group identity on drop.
    #[test]
    fn completed_pty_terminator_is_disarmed_before_drop() {
        let kills = Arc::new(AtomicUsize::new(0));
        drop(PtyTerminator {
            killer: Box::new(CountingKiller(Arc::clone(&kills))),
            process_tree: PlatformProcessTree::Unix(None),
            completion: Arc::new(Mutex::new(true)),
        });

        assert_eq!(kills.load(Ordering::SeqCst), 0);
    }
}
