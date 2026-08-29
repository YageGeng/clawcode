mod pipe;
mod pty;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use super::{TerminalError, TerminalSpawnRequest};

/// Final process status delivered after output readers have settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalExit {
    /// Numeric exit status when supplied by the platform.
    pub(crate) exit_code: Option<i32>,
    /// Sanitized watcher failure when process status could not be observed.
    pub(crate) failure: Option<String>,
}

/// Live process resources returned by one platform backend.
#[derive(typed_builder::TypedBuilder)]
pub(crate) struct SpawnedTerminal {
    /// Process control capability shared with the manager.
    pub(crate) control: Arc<dyn TerminalBackend>,
    /// Ordered chunks continuously drained from the process.
    pub(crate) output: mpsc::Receiver<Vec<u8>>,
    /// Final status sent after the process and readers settle.
    pub(crate) exit: oneshot::Receiver<TerminalExit>,
    /// Informational operating-system process identifier.
    #[builder(default)]
    pub(crate) os_pid: Option<u32>,
}

/// Cross-platform control surface retained after spawning a process.
#[async_trait]
pub(crate) trait TerminalBackend: Send + Sync {
    /// Writes arbitrary characters to an interactive process.
    async fn write(&self, chars: &str) -> Result<(), TerminalError>;

    /// Requests an interactive interrupt without removing the process.
    async fn interrupt(&self) -> Result<(), TerminalError>;

    /// Terminates the complete managed process tree and waits for acknowledgement.
    async fn terminate(&self) -> Result<(), TerminalError>;
}

/// Resolves the current platform shell and its command-source argument.
pub(crate) fn shell_command() -> (PathBuf, &'static str) {
    #[cfg(windows)]
    {
        (PathBuf::from("powershell.exe"), "-Command")
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt as _;

        if let Some(shell) = std::env::var_os("SHELL").map(PathBuf::from)
            && shell.is_absolute()
            && shell.is_file()
            && shell.metadata().is_ok_and(|metadata| {
                metadata.permissions().mode() & 0o111 != 0
            })
        {
            return (shell, "-c");
        }
        if Path::new("/bin/bash").is_file() {
            (PathBuf::from("/bin/bash"), "-c")
        } else {
            (PathBuf::from("sh"), "-c")
        }
    }
}

/// Spawns the pipe or native PTY backend selected by one validated request.
pub(crate) async fn spawn_backend(
    request: TerminalSpawnRequest,
) -> Result<SpawnedTerminal, TerminalError> {
    if request.tty {
        pty::spawn_pty(request).await
    } else {
        pipe::spawn_pipe(request).await
    }
}
